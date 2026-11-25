// JSON.parse per ECMA-404, a strict recursive descent that builds plain objects in source
// order, plus the reviver walk and JSON.stringify with toJSON, replacers and indentation

use super::heap::{Heap, ObjectHandle};
use super::object::{JsObject, ObjectClass, SHARED_KEYS, Shape, Slot, is_symbol_key, new_object};
use super::operations::{is_callable, is_primitive};
use super::*;
use std::borrow::Cow;

const MAX_DEPTH: usize = 512;

// powers of 10 representable exactly in binary64 (10^0..=10^15)
const POW10: [f64; 16] = [
    1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15,
];

struct JsonParser<'a> {
    bytes: &'a [u8],
    source: &'a str,
    position: usize,
    heap: Option<&'a Heap>,
    object_proto: Option<&'a ObjectHandle>,
    root_shape: Rc<Shape>,
}

pub(super) fn parse(source: &str) -> Result<Value, String> {
    parse_with(source, None, None)
}

fn parse_with(
    source: &str,
    heap: Option<&Heap>,
    object_proto: Option<&ObjectHandle>,
) -> Result<Value, String> {
    let mut parser = JsonParser {
        bytes: source.as_bytes(),
        source,
        position: 0,
        heap,
        object_proto,
        root_shape: Shape::root(),
    };
    parser.skip_whitespace();
    let value = parser.value(0)?;
    parser.skip_whitespace();
    if parser.position < parser.bytes.len() {
        return Err(parser.unexpected());
    }
    Ok(value)
}

#[inline]
fn hex_nibble(byte: u8) -> Option<u16> {
    match byte {
        b'0'..=b'9' => Some((byte - b'0') as u16),
        b'a'..=b'f' => Some((byte - b'a' + 10) as u16),
        b'A'..=b'F' => Some((byte - b'A' + 10) as u16),
        _ => None,
    }
}

#[inline]
fn reserve_slots(slots: &mut Vec<Slot>, shape: &Shape) {
    let len = slots.len();
    if len == slots.capacity() {
        let wanted = if len == 0 {
            shape.longest().clamp(1, 16)
        } else {
            let doubled = (len * 2).max(4);
            shape.longest().clamp(len + 1, doubled)
        };
        slots.reserve_exact(wanted - len);
    }
}

impl<'a> JsonParser<'a> {
    #[inline]
    fn skip_whitespace(&mut self) {
        let bytes = self.bytes;
        let len = bytes.len();
        let mut pos = self.position;
        while pos < len && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }
        self.position = pos;
    }

    #[cold]
    fn unexpected(&self) -> String {
        match self.source[self.position..].chars().next() {
            Some(character) => format!(
                "Unexpected token '{character}' in JSON at position {}",
                self.position
            ),
            None => "Unexpected end of JSON input".to_string(),
        }
    }

    #[inline]
    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.bytes.get(self.position) == Some(&byte) {
            self.position += 1;
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    #[inline]
    fn literal(&mut self, word: &[u8], value: Value) -> Result<Value, String> {
        if self.bytes[self.position..].starts_with(word) {
            self.position += word.len();
            Ok(value)
        } else {
            Err(self.unexpected())
        }
    }

    #[inline]
    fn build_object(&self, shape: Rc<Shape>, slots: Vec<Slot>) -> JsObject {
        match self.object_proto {
            Some(proto) => JsObject::from_shape(shape, slots, Some(proto.clone())),
            None => JsObject::from_shape_orphan(shape, slots),
        }
    }

    #[inline]
    fn alloc_object(&self, object: JsObject) -> Value {
        match self.heap {
            Some(heap) => Value::Object(heap.alloc_object_fast(object)),
            None => Value::Object(new_object(object)),
        }
    }

    #[inline]
    fn alloc_array(&self, values: Vec<Value>) -> Value {
        match self.heap {
            Some(heap) => Value::Array(heap.alloc_array_fast(values.into())),
            None => Value::Array(super::heap::alloc_array(values.into())),
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err("JSON nesting exceeds the depth limit".to_string());
        }
        match self.bytes.get(self.position) {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.scan_str().map(|text| match text {
                Cow::Borrowed(slice) => Value::String(JsStr::new(slice)),
                Cow::Owned(owned) => Value::String(JsStr::from(owned)),
            }),
            Some(b't') => self.literal(b"true", Value::Bool(true)),
            Some(b'f') => self.literal(b"false", Value::Bool(false)),
            Some(b'n') => self.literal(b"null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.unexpected()),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, String> {
        self.position += 1;
        self.skip_whitespace();
        if self.bytes.get(self.position) == Some(&b'}') {
            self.position += 1;
            let object = self.build_object(self.root_shape.clone(), Vec::new());
            return Ok(self.alloc_object(object));
        }
        enum ObjBuilder {
            Shared(Rc<Shape>, Vec<Slot>),
            Own(JsObject),
        }
        let mut builder = ObjBuilder::Shared(self.root_shape.clone(), Vec::new());
        loop {
            self.skip_whitespace();
            if self.bytes.get(self.position) != Some(&b'"') {
                return Err(self.unexpected());
            }
            let key = self.scan_str()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            let value = self.value(depth + 1)?;
            match &mut builder {
                ObjBuilder::Shared(shape, slots) => {
                    if shape.len() < SHARED_KEYS
                        && let Some(next) = shape.transition_data_existing(&key)
                    {
                        *shape = next;
                        reserve_slots(slots, shape);
                        slots.push(Slot::Data(value));
                    } else if let Some(pos) = shape.key_position(&key) {
                        slots[pos] = Slot::Data(value);
                    } else if shape.len() < SHARED_KEYS {
                        *shape = shape.transition_data(&key);
                        reserve_slots(slots, shape);
                        slots.push(Slot::Data(value));
                    } else {
                        let owned_shape = shape.clone();
                        let owned_slots = std::mem::take(slots);
                        let mut object = self.build_object(owned_shape, owned_slots);
                        object.insert(key.into_owned(), value);
                        builder = ObjBuilder::Own(object);
                    }
                }
                ObjBuilder::Own(object) => {
                    object.insert(key.into_owned(), value);
                }
            }
            self.skip_whitespace();
            match self.bytes.get(self.position) {
                Some(b',') => self.position += 1,
                Some(b'}') => {
                    self.position += 1;
                    let object = match builder {
                        ObjBuilder::Shared(shape, slots) => self.build_object(shape, slots),
                        ObjBuilder::Own(object) => object,
                    };
                    return Ok(self.alloc_object(object));
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
            return Ok(self.alloc_array(values));
        }
        loop {
            self.skip_whitespace();
            values.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.bytes.get(self.position) {
                Some(b',') => self.position += 1,
                Some(b']') => {
                    self.position += 1;
                    return Ok(self.alloc_array(values));
                }
                _ => return Err(self.unexpected()),
            }
        }
    }

    fn hex4(&mut self) -> Result<u16, String> {
        let bytes = self
            .bytes
            .get(self.position..self.position + 4)
            .ok_or_else(|| "Bad Unicode escape in JSON".to_string())?;
        let (Some(a), Some(b), Some(c), Some(d)) = (
            hex_nibble(bytes[0]),
            hex_nibble(bytes[1]),
            hex_nibble(bytes[2]),
            hex_nibble(bytes[3]),
        ) else {
            return Err("Bad Unicode escape in JSON".to_string());
        };
        self.position += 4;
        Ok((a << 12) | (b << 8) | (c << 4) | d)
    }

    #[inline]
    fn scan_str(&mut self) -> Result<Cow<'a, str>, String> {
        self.position += 1;
        let start = self.position;
        let bytes = self.bytes;
        let len = bytes.len();
        let mut pos = start;
        while pos < len {
            let byte = bytes[pos];
            if byte == b'"' {
                let slice = &self.source[start..pos];
                self.position = pos + 1;
                return Ok(Cow::Borrowed(slice));
            }
            if byte == b'\\' || byte < 0x20 {
                self.position = pos;
                if byte < 0x20 {
                    return Err("Bad control character in string literal in JSON".to_string());
                }
                return self.scan_str_escaped(start);
            }
            pos += 1;
        }
        self.position = pos;
        Err("Unterminated string in JSON".to_string())
    }

    #[cold]
    fn scan_str_escaped(&mut self, start: usize) -> Result<Cow<'a, str>, String> {
        let mut text = String::with_capacity((self.position - start) + 16);
        text.push_str(&self.source[start..self.position]);
        loop {
            match self.bytes.get(self.position) {
                Some(b'"') => {
                    self.position += 1;
                    return Ok(Cow::Owned(text));
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
                            if (0xd800..0xdc00).contains(&unit) {
                                if self.bytes[self.position..].starts_with(b"\\u") {
                                    let saved = self.position;
                                    self.position += 2;
                                    let low = self.hex4()?;
                                    if (0xdc00..0xe000).contains(&low) {
                                        let scalar = 0x10000
                                            + (((unit as u32 - 0xd800) << 10)
                                                | (low as u32 - 0xdc00));
                                        text.push(
                                            char::from_u32(scalar)
                                                .unwrap_or(char::REPLACEMENT_CHARACTER),
                                        );
                                    } else {
                                        self.position = saved;
                                        text.push(char::REPLACEMENT_CHARACTER);
                                    }
                                } else {
                                    text.push(char::REPLACEMENT_CHARACTER);
                                }
                            } else if (0xdc00..0xe000).contains(&unit) {
                                text.push(char::REPLACEMENT_CHARACTER);
                            } else {
                                text.push(
                                    char::from_u32(unit as u32)
                                        .unwrap_or(char::REPLACEMENT_CHARACTER),
                                );
                            }
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
            let chunk_start = self.position;
            let bytes = self.bytes;
            let len = bytes.len();
            let mut pos = chunk_start;
            while pos < len {
                let byte = bytes[pos];
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                pos += 1;
            }
            self.position = pos;
            if pos > chunk_start {
                text.push_str(&self.source[chunk_start..pos]);
            }
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.position;
        let bytes = self.bytes;
        let len = bytes.len();
        let mut pos = start;
        let neg = if pos < len && bytes[pos] == b'-' {
            pos += 1;
            true
        } else {
            false
        };
        let mut mantissa: u64 = 0;
        let mut digits: usize;
        match bytes.get(pos) {
            Some(b'0') => {
                pos += 1;
                digits = 1;
            }
            Some(&b @ b'1'..=b'9') => {
                mantissa = (b - b'0') as u64;
                digits = 1;
                pos += 1;
                while pos < len && bytes[pos].is_ascii_digit() {
                    if digits < 16 {
                        mantissa = mantissa * 10 + (bytes[pos] - b'0') as u64;
                    }
                    digits += 1;
                    pos += 1;
                }
            }
            _ => {
                self.position = pos;
                return Err(self.unexpected());
            }
        }
        let mut frac_digits: usize = 0;
        let mut has_exp = false;
        if pos < len && bytes[pos] == b'.' {
            pos += 1;
            if pos >= len || !bytes[pos].is_ascii_digit() {
                self.position = pos;
                return Err(self.unexpected());
            }
            while pos < len && bytes[pos].is_ascii_digit() {
                if digits < 16 {
                    mantissa = mantissa * 10 + (bytes[pos] - b'0') as u64;
                }
                digits += 1;
                frac_digits += 1;
                pos += 1;
            }
        }
        if pos < len && matches!(bytes[pos], b'e' | b'E') {
            has_exp = true;
            pos += 1;
            if pos < len && matches!(bytes[pos], b'+' | b'-') {
                pos += 1;
            }
            if pos >= len || !bytes[pos].is_ascii_digit() {
                self.position = pos;
                return Err(self.unexpected());
            }
            while pos < len && bytes[pos].is_ascii_digit() {
                pos += 1;
            }
        }
        self.position = pos;
        // single ieee-754 division is bit-exact when mantissa < 2^53 and 10^frac <= 10^15
        if !has_exp && digits <= 15 && frac_digits <= 15 {
            let value = if frac_digits == 0 {
                mantissa as f64
            } else {
                (mantissa as f64) / POW10[frac_digits]
            };
            return Ok(Value::Number(if neg { -value } else { value }));
        }
        self.source[start..self.position]
            .parse::<f64>()
            .map(Value::Number)
            .map_err(|_| self.unexpected())
    }
}

// JSON.stringify's string quoting
pub(crate) fn quote(text: &str) -> String {
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
        Value::Object(object) => Some(object.id()),
        Value::Array(array) => Some(array.id()),
        _ => None,
    }
}

impl ScriptRuntime {
    pub(super) fn parse_json_str(&mut self, source: &str) -> Result<Value, ScriptError> {
        let _heap = super::heap::enter(self.heap_id);
        let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
        parse_with(source, Some(heap), Some(&self.intrinsics.object_prototype))
            .map_err(|message| self.throw_error("SyntaxError", message))
    }

    pub(super) fn json_parse(&mut self, text: Value, reviver: Value) -> Result<Value, ScriptError> {
        let _heap = super::heap::enter(self.heap_id);
        let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
        let parsed = match text {
            Value::String(text) => {
                parse_with(&text, Some(heap), Some(&self.intrinsics.object_prototype))
            }
            other => {
                let text = self.coerce_string(other)?;
                parse_with(&text, Some(heap), Some(&self.intrinsics.object_prototype))
            }
        };
        let value = parsed.map_err(|message| self.throw_error("SyntaxError", message))?;
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
            // an array behind a proxy is walked by index too, as IsArray sees through proxies
            Value::Object(_) if self.is_array(&value)? => {
                let length = self.get_property(&value, "length")?;
                let length = self.coerce_number(length)?.max(0.0) as usize;
                for index in 0..length {
                    let name = index.to_string();
                    let element = self.internalize(value.clone(), name.clone(), reviver)?;
                    if matches!(element, Value::Undefined) {
                        self.delete_property(&value, &name, false)?;
                    } else {
                        self.set_property(&value, &name, element, false)?;
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

    // a frozen, prototype-less object holding JSON text for a primitive, as JSON.rawJSON gives
    pub(super) fn raw_json(&mut self, text: Value) -> Result<Value, ScriptError> {
        let text = self.coerce_string(text)?;
        let padded = |character: Option<char>| matches!(character, Some('\t' | '\n' | '\r' | ' '));
        if text.is_empty() || padded(text.chars().next()) || padded(text.chars().last()) {
            return Err(self.throw_error("SyntaxError", "Invalid value for JSON.rawJSON"));
        }
        let parsed = self.json_parse(Value::String(text.as_str().into()), Value::Undefined)?;
        if !is_primitive(&parsed) {
            return Err(self.throw_error(
                "SyntaxError",
                "JSON.rawJSON cannot hold an object or an array",
            ));
        }
        let mut object = JsObject::with_prototype(None, ObjectClass::RawJson);
        object.insert("rawJSON".to_string(), Value::String(text.into()));
        object.freeze(false);
        Ok(Value::Object(new_object(object)))
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
        if let Value::Object(object) = &value
            && matches!(object.borrow().class, ObjectClass::RawJson)
        {
            let text = self.get_property(&value, "rawJSON")?;
            return Ok(Some(self.coerce_string(text)?));
        }
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
            ref array if self.is_array(array)? => Some(self.serialize_array(state, &value)?),
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
            proxy => {
                let length = self.get_property(proxy, "length")?;
                let length = self.coerce_number(length)?;
                if length.is_nan() || length <= 0.0 {
                    0
                } else {
                    length.min(MAX_RUNTIME_ARRAY_LENGTH as f64) as usize
                }
            }
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

    #[test]
    fn fast_json_parser_handles_shapes_numbers_duplicates_and_surrogates() {
        // repeated schema objects share the exact same shape id
        let rows = parse(r#"[{"id":1,"name":"a"},{"id":2,"name":"b"},{"id":3,"name":"c"}]"#)
            .expect("rows");
        let Value::Array(arr) = rows else {
            panic!("expected array");
        };
        let arr_ref = arr.borrow();
        let shape_ids: Vec<u32> = arr_ref
            .iter()
            .map(|v| match v {
                Value::Object(o) => o.borrow().shape_id,
                _ => panic!("expected object"),
            })
            .collect();
        assert_ne!(shape_ids[0], 0);
        assert_eq!(shape_ids[0], shape_ids[1]);
        assert_eq!(shape_ids[1], shape_ids[2]);

        // duplicate keys overwrite in place without changing key order
        let dup = parse(r#"{"a":1,"b":2,"a":3,"__proto__":9}"#).expect("dup");
        let Value::Object(dup_obj) = dup else {
            panic!("expected object");
        };
        let dup_ref = dup_obj.borrow();
        assert_eq!(dup_ref.keys().collect::<Vec<_>>(), ["a", "b", "__proto__"]);
        assert!(matches!(dup_ref.get("a"), Some(Value::Number(n)) if *n == 3.0));
        assert!(matches!(dup_ref.get("__proto__"), Some(Value::Number(n)) if *n == 9.0));

        // >64 keys transitions cleanly to own shape without losing keys
        let big_entries: Vec<String> = (0..80).map(|i| format!(r#""k{i}":{i}"#)).collect();
        let big_json = format!("{{{}}}", big_entries.join(","));
        let big = parse(&big_json).expect("big object");
        let Value::Object(big_obj) = big else {
            panic!("expected object");
        };
        let big_ref = big_obj.borrow();
        assert_eq!(big_ref.keys().count(), 80);
        assert!(matches!(big_ref.get("k0"), Some(Value::Number(n)) if *n == 0.0));
        assert!(matches!(big_ref.get("k79"), Some(Value::Number(n)) if *n == 79.0));

        // negative zero, fast integers, exact decimals, and fallback exponents
        for (src, check_neg_zero) in [
            ("-0", true),
            ("-0.0", true),
            ("0", false),
            ("0.1", false),
            ("12.345", false),
            ("-0.0005", false),
            ("999999999999999", false),
            ("9007199254740993", false),
            ("0.0000000000000001", false),
            ("1.23e-10", false),
        ] {
            let Value::Number(actual) = parse(src).expect(src) else {
                panic!("expected number for {src}");
            };
            let expected: f64 = src.parse().unwrap();
            assert_eq!(actual.to_bits(), expected.to_bits(), "mismatch on {src}");
            if check_neg_zero {
                assert!(actual.is_sign_negative());
            }
        }

        // surrogate pairs and unpaired surrogates
        let Value::String(s) =
            parse(r#""\ud83d\ude00|\ud800|\udc00|\ud800\u0041""#).expect("surrogates")
        else {
            panic!("expected string");
        };
        assert_eq!(&*s, "\u{1f600}|\u{fffd}|\u{fffd}|\u{fffd}A");

        // runtime JSON.parse attaches real Object.prototype and enables JIT missing ICs
        let document = crate::dom::Document::parse("<p id='out'></p>").expect("doc");
        let document = Rc::new(RefCell::new(document));
        let mut rt = ScriptRuntime::new(document.clone());
        rt.execute(
            r#"
            const raw = '[{"a":1,"b":"ok"},{"a":2,"b":"yes","__proto__":7}]';
            let hits = 0;
            for (let i = 0; i < 30; i++) {
                const parsed = JSON.parse(raw);
                if (Object.getPrototypeOf(parsed[0]) !== Object.prototype) throw new Error("proto0");
                if (Object.getPrototypeOf(parsed[1]) !== Object.prototype) throw new Error("proto1");
                if (parsed[1]["__proto__"] !== 7) throw new Error("ownproto");
                if (parsed[0].missing !== undefined) throw new Error("missing");
                hits += parsed[0].a + parsed[1].a;
            }
            document.getElementById("out").textContent = String(hits);
            "#,
        )
        .expect("runtime json parse");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some("90")
        );
    }
}
