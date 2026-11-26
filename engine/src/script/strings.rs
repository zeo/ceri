// String.prototype with UTF-16 index semantics over the engine's UTF-8 strings: indexes,
// lengths and code units count UTF-16 units as JavaScript does, with an ASCII fast path

use super::operations::is_callable;
use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) enum StringMethod {
    At,
    CharAt,
    CharCodeAt,
    CodePointAt,
    Concat,
    EndsWith,
    Includes,
    IndexOf,
    IsWellFormed,
    LastIndexOf,
    LocaleCompare,
    Match,
    Normalize,
    MatchAll,
    PadEnd,
    PadStart,
    Repeat,
    Replace,
    ReplaceAll,
    Search,
    Slice,
    Split,
    StartsWith,
    Substr,
    Substring,
    ToLowerCase,
    ToUpperCase,
    ToWellFormed,
    Trim,
    TrimEnd,
    TrimStart,
    Iterator,
    // an annex B method that wraps a string in a tag, by its place in HTML_METHODS
    Html(u8),
}

pub(super) const STRING_METHODS: &[(&str, StringMethod, u32)] = &[
    ("at", StringMethod::At, 1),
    ("charAt", StringMethod::CharAt, 1),
    ("charCodeAt", StringMethod::CharCodeAt, 1),
    ("codePointAt", StringMethod::CodePointAt, 1),
    ("concat", StringMethod::Concat, 1),
    ("endsWith", StringMethod::EndsWith, 1),
    ("includes", StringMethod::Includes, 1),
    ("indexOf", StringMethod::IndexOf, 1),
    ("isWellFormed", StringMethod::IsWellFormed, 0),
    ("lastIndexOf", StringMethod::LastIndexOf, 1),
    ("localeCompare", StringMethod::LocaleCompare, 1),
    ("match", StringMethod::Match, 1),
    ("matchAll", StringMethod::MatchAll, 1),
    ("normalize", StringMethod::Normalize, 0),
    ("padEnd", StringMethod::PadEnd, 1),
    ("padStart", StringMethod::PadStart, 1),
    ("repeat", StringMethod::Repeat, 1),
    ("replace", StringMethod::Replace, 2),
    ("replaceAll", StringMethod::ReplaceAll, 2),
    ("search", StringMethod::Search, 1),
    ("slice", StringMethod::Slice, 2),
    ("split", StringMethod::Split, 2),
    ("startsWith", StringMethod::StartsWith, 1),
    ("substr", StringMethod::Substr, 2),
    ("substring", StringMethod::Substring, 2),
    ("toLowerCase", StringMethod::ToLowerCase, 0),
    ("toLocaleLowerCase", StringMethod::ToLowerCase, 0),
    ("toUpperCase", StringMethod::ToUpperCase, 0),
    ("toLocaleUpperCase", StringMethod::ToUpperCase, 0),
    ("toWellFormed", StringMethod::ToWellFormed, 0),
    ("trim", StringMethod::Trim, 0),
    ("trimEnd", StringMethod::TrimEnd, 0),
    ("trimRight", StringMethod::TrimEnd, 0),
    ("trimStart", StringMethod::TrimStart, 0),
    ("trimLeft", StringMethod::TrimStart, 0),
    ("@@iterator", StringMethod::Iterator, 0),
    ("anchor", StringMethod::Html(0), 1),
    ("big", StringMethod::Html(1), 0),
    ("blink", StringMethod::Html(2), 0),
    ("bold", StringMethod::Html(3), 0),
    ("fixed", StringMethod::Html(4), 0),
    ("fontcolor", StringMethod::Html(5), 1),
    ("fontsize", StringMethod::Html(6), 1),
    ("italics", StringMethod::Html(7), 0),
    ("link", StringMethod::Html(8), 1),
    ("small", StringMethod::Html(9), 0),
    ("strike", StringMethod::Html(10), 0),
    ("sub", StringMethod::Html(11), 0),
    ("sup", StringMethod::Html(12), 0),
];

// each annex B HTML method's tag and the attribute its argument fills
const HTML_METHODS: &[(&str, Option<&str>)] = &[
    ("a", Some("name")),
    ("big", None),
    ("blink", None),
    ("b", None),
    ("tt", None),
    ("font", Some("color")),
    ("font", Some("size")),
    ("i", None),
    ("a", Some("href")),
    ("small", None),
    ("strike", None),
    ("sub", None),
    ("sup", None),
];

// annex B escape: the unreserved ASCII stays, other units become %XX or %uXXXX
pub(super) fn escape(units: &[u16]) -> String {
    let mut out = String::with_capacity(units.len());
    for &unit in units {
        match u8::try_from(unit) {
            Ok(byte) if byte.is_ascii_alphanumeric() || b"@*_+-./".contains(&byte) => {
                out.push(char::from(byte))
            }
            Ok(byte) => out.push_str(&format!("%{byte:02X}")),
            Err(_) => out.push_str(&format!("%u{unit:04X}")),
        }
    }
    out
}

pub(super) fn unescape(units: &[u16]) -> String {
    let hex = |slice: &[u16]| {
        slice.iter().try_fold(0_u16, |value, &unit| {
            char::from_u32(u32::from(unit))?
                .to_digit(16)
                .map(|digit| value * 16 + digit as u16)
        })
    };
    let mut out = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        let decoded = (unit == u16::from(b'%'))
            .then(|| {
                if units.get(index + 1) == Some(&u16::from(b'u')) {
                    units
                        .get(index + 2..index + 6)
                        .and_then(hex)
                        .map(|value| (value, 6))
                } else {
                    units
                        .get(index + 1..index + 3)
                        .and_then(hex)
                        .map(|value| (value, 3))
                }
            })
            .flatten();
        match decoded {
            Some((value, length)) => {
                out.push(value);
                index += length;
            }
            None => {
                out.push(unit);
                index += 1;
            }
        }
    }
    from_units(&out)
}

// WhiteSpace and LineTerminator from the specification
pub(super) fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

// UTF-16 code units of a string; ASCII text maps one byte to one unit
pub(super) fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

pub(super) fn utf16_length(text: &str) -> usize {
    if text.is_ascii() {
        text.len()
    } else {
        text.encode_utf16().count()
    }
}

// the one-unit string at a UTF-16 index, as string indexing reads it
pub(super) fn unit_at(text: &str, index: usize) -> Option<Value> {
    if text.is_ascii() {
        return text
            .get(index..index + 1)
            .map(|unit| Value::String(unit.into()));
    }
    text.encode_utf16()
        .nth(index)
        .map(|unit| Value::String(String::from_utf16_lossy(&[unit]).into()))
}

pub(super) fn from_units(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

// the substring between two UTF-16 offsets as a JsStr without double-allocating on ASCII
fn utf16_slice_js(text: &JsStr, start: usize, end: usize) -> JsStr {
    if text.is_ascii() {
        let end = end.min(text.len());
        let start = start.min(end);
        if start == 0 && end == text.len() {
            return text.clone();
        }
        return JsStr::new(&text[start..end]);
    }
    let units = units(text);
    let end = end.min(units.len());
    let start = start.min(end);
    from_units(&units[start..end]).into()
}

// the substring between two UTF-16 offsets
#[cfg(test)]
fn utf16_slice(text: &str, start: usize, end: usize) -> String {
    if text.is_ascii() {
        let end = end.min(text.len());
        let start = start.min(end);
        return text[start..end].to_string();
    }
    let units = units(text);
    let end = end.min(units.len());
    let start = start.min(end);
    from_units(&units[start..end])
}

fn find_units(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return (from <= haystack.len()).then_some(from);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    (from..=haystack.len() - needle.len())
        .find(|&index| haystack[index..index + needle.len()] == *needle)
}

fn rfind_units(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    let last = (haystack.len() - needle.len()).min(from);
    (0..=last)
        .rev()
        .find(|&index| haystack[index..index + needle.len()] == *needle)
}

fn argument(arguments: &[Value], index: usize) -> Value {
    arguments.get(index).cloned().unwrap_or(Value::Undefined)
}

impl ScriptRuntime {
    #[inline]
    fn this_js_str(&mut self, this: &Value, method: StringMethod) -> Result<JsStr, ScriptError> {
        match this {
            Value::String(text) => Ok(text.clone()),
            Value::Undefined | Value::Null => Err(self.type_error(format!(
                "String.prototype.{method:?} called on null or undefined"
            ))),
            other => Ok(self.coerce_string(other.clone())?.into()),
        }
    }

    #[inline]
    fn arg_js_str(&mut self, value: Option<&Value>) -> Result<JsStr, ScriptError> {
        match value {
            Some(Value::String(text)) => Ok(text.clone()),
            Some(other) => Ok(self.coerce_string(other.clone())?.into()),
            None => Ok(JsStr::new("undefined")),
        }
    }

    #[inline]
    fn string_integer(&mut self, value: Option<&Value>, default: f64) -> Result<f64, ScriptError> {
        match value {
            None | Some(Value::Undefined) => Ok(default),
            Some(Value::Number(number)) => Ok(if number.is_nan() { 0.0 } else { number.trunc() }),
            Some(other) => {
                let number = self.coerce_number(other.clone())?;
                Ok(if number.is_nan() { 0.0 } else { number.trunc() })
            }
        }
    }

    // checks whether pattern is an unmodified RegExp whose @@split resolves to the intrinsic
    fn intrinsic_regexp_split(
        &self,
        heap: &super::heap::Heap,
        pattern: &Value,
    ) -> Option<Rc<super::pattern::RegExpData>> {
        let data = self.plain_regexp_data(heap, pattern)?;
        (self.regexp_proto_slot_intact(heap, 7, super::regexp::RegExpMethod::Split)
            && self.regexp_species_and_exec_intact(heap))
        .then_some(data)
    }

    pub(super) fn template_replacement(
        &mut self,
        replacement: &Value,
        matched: &[u16],
        position: usize,
        subject: &[u16],
        captures: &[Option<Vec<u16>>],
        groups: Value,
    ) -> Result<Vec<u16>, ScriptError> {
        if is_callable(replacement) {
            let mut arguments = vec![Value::String(from_units(matched).into())];
            for capture in captures {
                arguments.push(match capture {
                    Some(capture) => Value::String(from_units(capture).into()),
                    None => Value::Undefined,
                });
            }
            arguments.push(Value::Number(position as f64));
            arguments.push(Value::String(from_units(subject).into()));
            if !matches!(groups, Value::Undefined) {
                arguments.push(groups);
            }
            let result = self.call_function(replacement.clone(), Value::Undefined, arguments)?;
            return Ok(units(&self.coerce_string(result)?));
        }
        let template = units(&self.coerce_string(replacement.clone())?);
        let named = if matches!(groups, Value::Undefined) {
            None
        } else {
            Some(self.coerce_object(groups)?)
        };
        self.expand_substitution(
            &template,
            matched,
            position,
            subject,
            captures,
            named.as_ref(),
        )
    }

    // single-pass substitution for dollar escapes and named captures
    pub(super) fn expand_substitution(
        &mut self,
        template: &[u16],
        matched: &[u16],
        position: usize,
        subject: &[u16],
        captures: &[Option<Vec<u16>>],
        groups: Option<&Value>,
    ) -> Result<Vec<u16>, ScriptError> {
        let mut output = Vec::with_capacity(template.len());
        let mut index = 0;
        let dollar = u16::from(b'$');
        while index < template.len() {
            let unit = template[index];
            if unit != dollar || index + 1 >= template.len() {
                output.push(unit);
                index += 1;
                continue;
            }
            let next = template[index + 1];
            match next {
                n if n == dollar => {
                    output.push(dollar);
                    index += 2;
                }
                n if n == u16::from(b'&') => {
                    output.extend_from_slice(matched);
                    index += 2;
                }
                n if n == u16::from(b'`') => {
                    output.extend_from_slice(&subject[..position]);
                    index += 2;
                }
                n if n == u16::from(b'\'') => {
                    let end = (position + matched.len()).min(subject.len());
                    output.extend_from_slice(&subject[end..]);
                    index += 2;
                }
                n if n == u16::from(b'<') => {
                    if let Some(groups) = groups
                        && let Some(close) = template[index + 2..]
                            .iter()
                            .position(|u| *u == u16::from(b'>'))
                    {
                        let name = from_units(&template[index + 2..index + 2 + close]);
                        let value = self.get_property(groups, &name)?;
                        if !matches!(value, Value::Undefined) {
                            output.extend(units(&self.coerce_string(value)?));
                        }
                        index += close + 3;
                    } else {
                        output.push(dollar);
                        index += 1;
                    }
                }
                n if (u16::from(b'0')..=u16::from(b'9')).contains(&n) => {
                    let first = usize::from(n - u16::from(b'0'));
                    let second = template
                        .get(index + 2)
                        .filter(|u| (u16::from(b'0')..=u16::from(b'9')).contains(u))
                        .map(|u| usize::from(u - u16::from(b'0')));
                    let two = second.map(|second| first * 10 + second);
                    if let Some(two) = two.filter(|two| *two >= 1 && *two <= captures.len()) {
                        if let Some(capture) = &captures[two - 1] {
                            output.extend_from_slice(capture);
                        }
                        index += 3;
                    } else if first >= 1 && first <= captures.len() {
                        if let Some(capture) = &captures[first - 1] {
                            output.extend_from_slice(capture);
                        }
                        index += 2;
                    } else {
                        output.push(dollar);
                        index += 1;
                    }
                }
                _ => {
                    output.push(dollar);
                    index += 1;
                }
            }
        }
        Ok(output)
    }

    pub(super) fn call_string_method_slice(
        &mut self,
        method: StringMethod,
        this: &Value,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        use StringMethod as M;
        // regular-expression aware methods hand a pattern its own symbol protocol first
        if let M::Match | M::MatchAll | M::Replace | M::ReplaceAll | M::Search | M::Split = method {
            if matches!(this, Value::Undefined | Value::Null) {
                return Err(self.type_error(format!(
                    "String.prototype.{method:?} called on null or undefined"
                )));
            }
            if let Some(pattern) = arguments.first()
                && !super::operations::is_primitive(pattern)
            {
                let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
                if matches!(method, M::Split)
                    && let Some(data) = self.intrinsic_regexp_split(heap, pattern)
                {
                    let subject = self.this_js_str(this, method)?;
                    let limit = match arguments.get(1) {
                        None | Some(Value::Undefined) => u32::MAX as usize,
                        Some(limit) => self.coerce_uint32(limit.clone())? as usize,
                    };
                    return self.regexp_split_str_fast(heap, &data, &subject, limit);
                }
                if let Some((slot_idx, rx_method)) = match method {
                    M::Replace => Some((5, super::regexp::RegExpMethod::Replace)),
                    M::Match => Some((3, super::regexp::RegExpMethod::Match)),
                    M::Search => Some((6, super::regexp::RegExpMethod::Search)),
                    _ => None,
                } && self.plain_regexp_data(heap, pattern).is_some()
                    && self.regexp_proto_slot_intact(heap, slot_idx, rx_method)
                {
                    let mut forwarded = Vec::with_capacity(arguments.len());
                    forwarded.push(this.clone());
                    forwarded.extend(arguments.iter().skip(1).cloned());
                    return self.call_regexp(rx_method, pattern.clone(), forwarded, None);
                }
                let pattern = pattern.clone();
                if let M::ReplaceAll | M::MatchAll = method
                    && self.is_regexp(&pattern)?
                {
                    let flags = self.get_property(&pattern, "flags")?;
                    if matches!(flags, Value::Undefined | Value::Null) {
                        return Err(self.type_error("RegExp flags cannot be null or undefined"));
                    }
                    let flags = self.coerce_string(flags)?;
                    if !flags.contains('g') {
                        return Err(self.type_error(format!(
                            "{method:?} must be called with a global RegExp"
                        )));
                    }
                }
                let symbol = match method {
                    M::Match => "@@match",
                    M::MatchAll => "@@matchAll",
                    M::Replace | M::ReplaceAll => "@@replace",
                    M::Search => "@@search",
                    _ => "@@split",
                };
                let protocol = self.get_property(&pattern, symbol)?;
                if !matches!(protocol, Value::Undefined | Value::Null) {
                    if !is_callable(&protocol) {
                        return Err(self.type_error(format!(
                            "Symbol protocol for {method:?} is not a function"
                        )));
                    }
                    let mut forwarded = Vec::with_capacity(arguments.len());
                    forwarded.push(this.clone());
                    forwarded.extend(arguments.iter().skip(1).cloned());
                    return self.call_function(protocol, pattern, forwarded);
                }
            }
        }
        let text = self.this_js_str(this, method)?;
        Ok(match method {
            M::At | M::CharAt | M::CharCodeAt | M::CodePointAt => {
                let ascii = text.is_ascii();
                let length = if ascii {
                    text.len()
                } else {
                    utf16_length(&text)
                };
                let index = self.string_integer(arguments.first(), 0.0)?;
                let index = if matches!(method, M::At) && index < 0.0 {
                    length as f64 + index
                } else {
                    index
                };
                if index < 0.0 || index >= length as f64 {
                    return Ok(match method {
                        M::CharAt => Value::String(JsStr::new("")),
                        M::CharCodeAt => Value::Number(f64::NAN),
                        _ => Value::Undefined,
                    });
                }
                let index = index as usize;
                if ascii {
                    return Ok(match method {
                        M::CharCodeAt | M::CodePointAt => {
                            Value::Number(f64::from(text.as_bytes()[index]))
                        }
                        _ => Value::String(JsStr::new(&text[index..index + 1])),
                    });
                }
                let code_units = units(&text);
                match method {
                    M::CharCodeAt => Value::Number(f64::from(code_units[index])),
                    M::CodePointAt => {
                        let unit = code_units[index];
                        let point = if (0xd800..0xdc00).contains(&unit)
                            && let Some(&low) = code_units.get(index + 1)
                            && (0xdc00..0xe000).contains(&low)
                        {
                            0x10000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00)
                        } else {
                            u32::from(unit)
                        };
                        Value::Number(f64::from(point))
                    }
                    _ => Value::String(from_units(&code_units[index..index + 1]).into()),
                }
            }
            M::Concat => {
                if arguments.is_empty() {
                    return Ok(Value::String(text));
                }
                let mut result = text.to_string();
                for value in arguments {
                    match value {
                        Value::String(part) => result.push_str(part),
                        other => result.push_str(&self.coerce_string(other.clone())?),
                    }
                }
                Value::String(result.into())
            }
            M::Includes | M::StartsWith | M::EndsWith | M::IndexOf | M::LastIndexOf => {
                if let M::Includes | M::StartsWith | M::EndsWith = method
                    && let Some(first @ Value::Object(_)) = arguments.first()
                    && self.is_regexp(first)?
                {
                    return Err(self.type_error(format!(
                        "First argument to String.prototype.{method:?} must not be a regular expression"
                    )));
                }
                let search_str = self.arg_js_str(arguments.first())?;
                if text.is_ascii() && search_str.is_ascii() {
                    let length = text.len();
                    let hay: &str = &text;
                    let needle: &str = &search_str;
                    return Ok(match method {
                        M::Includes | M::IndexOf => {
                            let from = self.string_integer(arguments.get(1), 0.0)?;
                            let from = from.clamp(0.0, length as f64) as usize;
                            let found = hay[from..].find(needle).map(|off| from + off);
                            if matches!(method, M::Includes) {
                                Value::Bool(found.is_some())
                            } else {
                                Value::Number(found.map_or(-1.0, |index| index as f64))
                            }
                        }
                        M::LastIndexOf => {
                            let from = match arguments.get(1) {
                                None | Some(Value::Undefined) => length as f64,
                                Some(Value::Number(number)) => {
                                    if number.is_nan() {
                                        length as f64
                                    } else {
                                        number.trunc()
                                    }
                                }
                                Some(position) => {
                                    let number = self.coerce_number(position.clone())?;
                                    if number.is_nan() {
                                        length as f64
                                    } else {
                                        number.trunc()
                                    }
                                }
                            };
                            let from = from.clamp(0.0, length as f64) as usize;
                            let found = if needle.len() > length {
                                None
                            } else {
                                let last = (length - needle.len()).min(from);
                                hay[..last + needle.len()].rfind(needle)
                            };
                            Value::Number(found.map_or(-1.0, |index| index as f64))
                        }
                        M::StartsWith => {
                            let from = self.string_integer(arguments.get(1), 0.0)?;
                            let start = from.clamp(0.0, length as f64) as usize;
                            Value::Bool(hay[start..].starts_with(needle))
                        }
                        _ => {
                            let end = self.string_integer(arguments.get(1), length as f64)?;
                            let end = end.clamp(0.0, length as f64) as usize;
                            Value::Bool(hay[..end].ends_with(needle))
                        }
                    });
                }
                let search = units(&search_str);
                let haystack = units(&text);
                let length = haystack.len();
                match method {
                    M::Includes | M::IndexOf => {
                        let from = self.string_integer(arguments.get(1), 0.0)?;
                        let from = from.clamp(0.0, length as f64) as usize;
                        let found = find_units(&haystack, &search, from);
                        if matches!(method, M::Includes) {
                            Value::Bool(found.is_some())
                        } else {
                            Value::Number(found.map_or(-1.0, |index| index as f64))
                        }
                    }
                    M::LastIndexOf => {
                        let from = match arguments.get(1) {
                            None | Some(Value::Undefined) => length as f64,
                            Some(position) => {
                                let number = self.coerce_number(position.clone())?;
                                if number.is_nan() {
                                    length as f64
                                } else {
                                    number.trunc()
                                }
                            }
                        };
                        let from = from.clamp(0.0, length as f64) as usize;
                        Value::Number(
                            rfind_units(&haystack, &search, from)
                                .map_or(-1.0, |index| index as f64),
                        )
                    }
                    M::StartsWith => {
                        let from = self.string_integer(arguments.get(1), 0.0)?;
                        let start = from.clamp(0.0, length as f64) as usize;
                        Value::Bool(haystack[start..].starts_with(&search))
                    }
                    _ => {
                        let end = self.string_integer(arguments.get(1), length as f64)?;
                        let end = end.clamp(0.0, length as f64) as usize;
                        Value::Bool(haystack[..end].ends_with(&search))
                    }
                }
            }
            M::Normalize => {
                let form = match arguments.first() {
                    None | Some(Value::Undefined) => JsStr::new("NFC"),
                    Some(Value::String(s)) => s.clone(),
                    Some(form) => self.coerce_string(form.clone())?.into(),
                };
                let Some(form) = crate::unicode::NormalizationForm::parse(&form) else {
                    return Err(self.range_error(
                        "The normalization form should be one of NFC, NFD, NFKC, NFKD.",
                    ));
                };
                Value::String(crate::unicode::normalize(&text, form).into())
            }
            M::IsWellFormed => Value::Bool(true),
            M::ToWellFormed => Value::String(text),
            M::LocaleCompare => {
                let other = self.arg_js_str(arguments.first())?;
                let order = locale_compare(&text, &other);
                Value::Number(match order {
                    std::cmp::Ordering::Less => -1.0,
                    std::cmp::Ordering::Equal => 0.0,
                    std::cmp::Ordering::Greater => 1.0,
                })
            }
            M::PadStart | M::PadEnd => {
                let target = self.string_integer(arguments.first(), 0.0)?;
                let length = utf16_length(&text);
                let filler = match arguments.get(1) {
                    None | Some(Value::Undefined) => vec![u16::from(b' ')],
                    Some(filler) => units(&self.coerce_string(filler.clone())?),
                };
                if target <= length as f64 || filler.is_empty() {
                    return Ok(Value::String(text));
                }
                let needed = target as usize - length;
                if target as usize > MAX_RUNTIME_STRING_UNITS {
                    return Err(self.range_error("Invalid string length"));
                }
                let padding: Vec<u16> = filler.iter().copied().cycle().take(needed).collect();
                let padding = from_units(&padding);
                Value::String(
                    if matches!(method, M::PadStart) {
                        format!("{padding}{text}")
                    } else {
                        format!("{text}{padding}")
                    }
                    .into(),
                )
            }
            M::Repeat => {
                let count = self.string_integer(arguments.first(), 0.0)?;
                if !(0.0..f64::INFINITY).contains(&count) {
                    return Err(self
                        .range_error(format!("Invalid count value: {}", number_to_string(count))));
                }
                if (count as usize).saturating_mul(utf16_length(&text)) > MAX_RUNTIME_STRING_UNITS {
                    return Err(self.range_error("Invalid string length"));
                }
                Value::String(text.repeat(count as usize).into())
            }
            M::Replace | M::ReplaceAll => {
                let search_str = self.arg_js_str(arguments.first())?;
                let replacement = argument(arguments, 1);
                let functional = is_callable(&replacement);
                let replacement = if functional {
                    replacement
                } else {
                    let rep_str: JsStr = match &replacement {
                        Value::String(s) => s.clone(),
                        other => self.coerce_string(other.clone())?.into(),
                    };
                    if text.is_ascii()
                        && search_str.is_ascii()
                        && !search_str.is_empty()
                        && !rep_str.contains('$')
                    {
                        let hay: &str = &text;
                        let needle: &str = &search_str;
                        return Ok(if matches!(method, M::Replace) {
                            match hay.find(needle) {
                                None => Value::String(text),
                                Some(pos) => {
                                    let mut out = String::with_capacity(
                                        hay.len() - needle.len() + rep_str.len(),
                                    );
                                    out.push_str(&hay[..pos]);
                                    out.push_str(&rep_str);
                                    out.push_str(&hay[pos + needle.len()..]);
                                    Value::String(out.into())
                                }
                            }
                        } else if !hay.contains(needle) {
                            Value::String(text)
                        } else {
                            Value::String(hay.replace(needle, &rep_str).into())
                        });
                    }
                    Value::String(rep_str)
                };
                let search = units(&search_str);
                let subject = units(&text);
                let mut positions = Vec::new();
                match method {
                    M::Replace => positions.extend(find_units(&subject, &search, 0)),
                    _ => {
                        let step = search.len().max(1);
                        let mut from = 0;
                        while let Some(position) = find_units(&subject, &search, from) {
                            positions.push(position);
                            from = position + step;
                        }
                    }
                }
                if positions.is_empty() {
                    return Ok(Value::String(text));
                }
                let mut output = Vec::with_capacity(subject.len());
                let mut last = 0;
                for position in positions {
                    output.extend_from_slice(&subject[last..position]);
                    let replaced = self.template_replacement(
                        &replacement,
                        &search,
                        position,
                        &subject,
                        &[],
                        Value::Undefined,
                    )?;
                    output.extend(replaced);
                    last = position + search.len();
                }
                output.extend_from_slice(&subject[last..]);
                Value::String(from_units(&output).into())
            }
            M::Match | M::MatchAll | M::Search => {
                // a string pattern becomes a regular expression, as RegExp(pattern) would
                let pattern = match arguments.first() {
                    None | Some(Value::Undefined) => String::new(),
                    Some(other) => self.coerce_string(other.clone())?,
                };
                let flags = if matches!(method, M::MatchAll) {
                    "g"
                } else {
                    ""
                };
                let regexp = self.new_regexp(&pattern, flags)?;
                let symbol = match method {
                    M::Match => "@@match",
                    M::MatchAll => "@@matchAll",
                    _ => "@@search",
                };
                let protocol = self.get_property(&regexp, symbol)?;
                if !is_callable(&protocol) {
                    return Err(self.type_error("regular expressions are unavailable"));
                }
                self.call_function(protocol, regexp, vec![Value::String(text)])?
            }
            M::Slice | M::Substring | M::Substr => {
                let length = utf16_length(&text) as f64;
                let start = self.string_integer(arguments.first(), 0.0)?;
                let (start, end) = match method {
                    M::Slice => {
                        let end = self.string_integer(arguments.get(1), length)?;
                        let resolve = |value: f64| {
                            if value < 0.0 {
                                (length + value).max(0.0)
                            } else {
                                value.min(length)
                            }
                        };
                        (resolve(start), resolve(end))
                    }
                    M::Substring => {
                        let end = self.string_integer(arguments.get(1), length)?;
                        let start = start.clamp(0.0, length);
                        let end = end.clamp(0.0, length);
                        (start.min(end), start.max(end))
                    }
                    _ => {
                        let start = if start < 0.0 {
                            (length + start).max(0.0)
                        } else {
                            start.min(length)
                        };
                        let count = self.string_integer(arguments.get(1), length)?;
                        let end = (start + count.max(0.0)).min(length);
                        (start, end)
                    }
                };
                if start >= end {
                    return Ok(Value::String(JsStr::new("")));
                }
                Value::String(utf16_slice_js(&text, start as usize, end as usize))
            }
            M::Split => {
                let limit = match arguments.get(1) {
                    None | Some(Value::Undefined) => u32::MAX as usize,
                    Some(limit) => self.coerce_uint32(limit.clone())? as usize,
                };
                let separator_arg = arguments.first();
                let sep_str = match separator_arg {
                    None | Some(Value::Undefined) => None,
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(other) => Some(self.coerce_string(other.clone())?.into()),
                };
                if limit == 0 {
                    return Ok(Self::new_array(Vec::new()));
                }
                let parts: Vec<Value> = match sep_str {
                    None => vec![Value::String(text)],
                    Some(sep_str) => {
                        if text.is_ascii() && sep_str.is_ascii() {
                            if sep_str.is_empty() {
                                text.as_bytes()
                                    .iter()
                                    .take(limit)
                                    .enumerate()
                                    .map(|(i, _)| Value::String(JsStr::new(&text[i..i + 1])))
                                    .collect()
                            } else {
                                let mut parts = Vec::new();
                                let hay: &str = &text;
                                let needle: &str = &sep_str;
                                let mut last = 0;
                                while let Some(off) = hay[last..].find(needle) {
                                    let pos = last + off;
                                    parts.push(Value::String(JsStr::new(&hay[last..pos])));
                                    if parts.len() == limit {
                                        break;
                                    }
                                    last = pos + needle.len();
                                }
                                if parts.len() < limit {
                                    if last == 0 {
                                        parts.push(Value::String(text));
                                    } else {
                                        parts.push(Value::String(JsStr::new(&hay[last..])));
                                    }
                                }
                                parts
                            }
                        } else {
                            let separator = units(&sep_str);
                            let subject = units(&text);
                            if separator.is_empty() {
                                subject
                                    .iter()
                                    .take(limit)
                                    .map(|unit| Value::String(from_units(&[*unit]).into()))
                                    .collect()
                            } else {
                                let mut parts = Vec::new();
                                let mut last = 0;
                                while let Some(position) = find_units(&subject, &separator, last) {
                                    parts.push(Value::String(
                                        from_units(&subject[last..position]).into(),
                                    ));
                                    if parts.len() == limit {
                                        break;
                                    }
                                    last = position + separator.len();
                                }
                                if parts.len() < limit {
                                    parts.push(Value::String(from_units(&subject[last..]).into()));
                                }
                                parts
                            }
                        }
                    }
                };
                if parts.len() > MAX_RUNTIME_ARRAY_LENGTH {
                    return Err(
                        self.range_error("string split exceeds the deterministic array limit")
                    );
                }
                Self::new_array(parts)
            }
            M::ToLowerCase => {
                if text.is_ascii() && !text.bytes().any(|b| b.is_ascii_uppercase()) {
                    Value::String(text)
                } else {
                    Value::String(text.to_lowercase().into())
                }
            }
            M::ToUpperCase => {
                if text.is_ascii() && !text.bytes().any(|b| b.is_ascii_lowercase()) {
                    Value::String(text)
                } else {
                    Value::String(text.to_uppercase().into())
                }
            }
            M::Trim => {
                let trimmed = text.trim_matches(is_js_whitespace);
                if trimmed.len() == text.len() {
                    Value::String(text)
                } else {
                    Value::String(JsStr::new(trimmed))
                }
            }
            M::TrimStart => {
                let trimmed = text.trim_start_matches(is_js_whitespace);
                if trimmed.len() == text.len() {
                    Value::String(text)
                } else {
                    Value::String(JsStr::new(trimmed))
                }
            }
            M::TrimEnd => {
                let trimmed = text.trim_end_matches(is_js_whitespace);
                if trimmed.len() == text.len() {
                    Value::String(text)
                } else {
                    Value::String(JsStr::new(trimmed))
                }
            }
            M::Html(index) => {
                let (tag, attribute) = HTML_METHODS[usize::from(index)];
                let attribute = match attribute {
                    Some(name) => format!(
                        " {name}=\"{}\"",
                        self.coerce_string(argument(arguments, 0))?
                            .replace('"', "&quot;")
                    ),
                    None => String::new(),
                };
                Value::String(format!("<{tag}{attribute}>{text}</{tag}>").into())
            }
            M::Iterator => Value::Iterator(Rc::new(RefCell::new(IteratorData {
                values: text
                    .chars()
                    .map(|character| Value::String(character.to_string().into()))
                    .collect(),
                index: 0,
            }))),
        })
    }

    // IsRegExp: an object with a truthy Symbol.match, or a RegExp instance
    pub(super) fn is_regexp(&mut self, value: &Value) -> Result<bool, ScriptError> {
        if !matches!(value, Value::Object(_)) {
            return Ok(false);
        }
        let matcher = self.get_property(value, "@@match")?;
        if !matches!(matcher, Value::Undefined) {
            return Ok(matcher.truthy());
        }
        Ok(
            matches!(value, Value::Object(object) if matches!(object.borrow().class, object::ObjectClass::RegExp(_))),
        )
    }
}

// a locale-insensitive collation: letters compare case-insensitively first, then lowercase
// before uppercase, as the root collation orders them
pub(super) fn locale_compare(left: &str, right: &str) -> std::cmp::Ordering {
    if left.is_ascii() && right.is_ascii() {
        let (lb, rb) = (left.as_bytes(), right.as_bytes());
        let mut tie = std::cmp::Ordering::Equal;
        let min_len = lb.len().min(rb.len());
        for i in 0..min_len {
            let (a, b) = (lb[i], rb[i]);
            if a == b {
                continue;
            }
            let (la, lb_c) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
            if la != lb_c {
                return la.cmp(&lb_c);
            }
            if tie == std::cmp::Ordering::Equal {
                tie = if a.is_ascii_lowercase() {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                };
            }
        }
        return lb.len().cmp(&rb.len()).then(tie);
    }
    let folded = left.chars().flat_map(char::to_lowercase);
    folded
        .cmp(right.chars().flat_map(char::to_lowercase))
        .then_with(|| {
            left.chars()
                .zip(right.chars())
                .find(|(a, b)| a != b)
                .map(|(a, b)| match (a.is_lowercase(), b.is_lowercase()) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.cmp(&b),
                })
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

// CSS.escape, the cssom's serialization of an identifier
pub(super) fn css_escape(text: &str) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    let mut escaped = String::with_capacity(text.len());
    for (index, &character) in characters.iter().enumerate() {
        let digit = character.is_ascii_digit();
        match character {
            '\0' => escaped.push('\u{FFFD}'),
            '\u{1}'..='\u{1F}' | '\u{7F}' => {
                escaped.push_str(&format!("\\{:x} ", character as u32))
            }
            _ if digit && (index == 0 || (index == 1 && characters[0] == '-')) => {
                escaped.push_str(&format!("\\{:x} ", character as u32));
            }
            '-' if index == 0 && characters.len() == 1 => escaped.push_str("\\-"),
            _ if !character.is_ascii()
                || character == '-'
                || character == '_'
                || character.is_ascii_alphanumeric() =>
            {
                escaped.push(character);
            }
            _ => {
                escaped.push('\\');
                escaped.push(character);
            }
        }
    }
    escaped
}

// the characters encodeURI leaves as they are beyond the ones encodeURIComponent does
pub(super) const URI_RESERVED: &str = ";/?:@&=+$,#";

// encodeURIComponent, or encodeURI with the reserved characters kept
pub(super) fn encode_uri(text: &str, keep: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii_alphanumeric()
            || "-_.!~*'()".contains(character)
            || keep.contains(character)
        {
            encoded.push(character);
            continue;
        }
        for byte in character.encode_utf8(&mut [0; 4]).bytes() {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

// decodeURIComponent, or decodeURI with escapes of the reserved characters left escaped; None
// when an escape is malformed or the bytes are not UTF-8
pub(super) fn decode_uri(text: &str, keep: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let byte_at = |index: usize| -> Option<u8> {
        if bytes.get(index) != Some(&b'%') {
            return None;
        }
        u8::from_str_radix(text.get(index + 1..index + 3)?, 16).ok()
    };
    let mut decoded = String::with_capacity(text.len());
    let mut index = 0;
    while let Some(offset) = text[index..].find('%') {
        decoded.push_str(&text[index..index + offset]);
        index += offset;
        let lead = byte_at(index)?;
        let length = match lead.leading_ones() {
            0 => 1,
            count @ 2..=4 => count as usize,
            _ => return None,
        };
        let mut sequence = vec![lead];
        for part in 1..length {
            sequence.push(byte_at(index + part * 3)?);
        }
        let character = std::str::from_utf8(&sequence).ok()?;
        match keep.contains(character) && length == 1 {
            true => decoded.push_str(&text[index..index + 3]),
            false => decoded.push_str(character),
        }
        index += length * 3;
    }
    decoded.push_str(&text[index..]);
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_functions_escape_and_unescape_like_the_spec() {
        assert_eq!(
            encode_uri("a b/é#?😀", ""),
            "a%20b%2F%C3%A9%23%3F%F0%9F%98%80"
        );
        assert_eq!(encode_uri("a b/é#?", URI_RESERVED), "a%20b/%C3%A9#?");
        assert_eq!(
            decode_uri("a%20b%2F%C3%A9%F0%9F%98%80", "").as_deref(),
            Some("a b/é😀")
        );
        assert_eq!(
            decode_uri("%2F%20%23", URI_RESERVED).as_deref(),
            Some("%2F %23")
        );
        for malformed in [
            "%",
            "%2",
            "%zz",
            "%C3",
            "%C3%28",
            "%80",
            "%ED%A0%80",
            "%C0%AF",
        ] {
            assert_eq!(decode_uri(malformed, ""), None, "{malformed}");
        }
    }

    #[test]
    fn replacement_templates_expand_like_the_spec() {
        let document = crate::dom::Document::parse(r#"<p></p>"#).expect("document should parse");
        let mut runtime = ScriptRuntime::new(Rc::new(RefCell::new(document)));
        let subject = units("abc");
        let output = runtime
            .expand_substitution(
                &units("[$$|$&|$`|$'|$1|$9]"),
                &units("b"),
                1,
                &subject,
                &[Some(units("x"))],
                None,
            )
            .expect("template should expand");
        assert_eq!(from_units(&output), "[$|b|a|c|x|$9]");
    }

    #[test]
    fn utf16_offsets_cover_astral_characters() {
        assert_eq!(utf16_length("a\u{1f600}b"), 4);
        assert_eq!(utf16_slice("a\u{1f600}b", 1, 3), "\u{1f600}");
        assert_eq!(utf16_slice("hello", 1, 3), "el");
    }
}
