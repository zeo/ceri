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
];

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

// the substring between two UTF-16 offsets
pub(super) fn utf16_slice(text: &str, start: usize, end: usize) -> String {
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

// expands $$, $&, $`, $' and $n in a replacement template for a string match
fn expand_replacement(
    template: &[u16],
    matched: &[u16],
    position: usize,
    subject: &[u16],
    captures: &[Option<Vec<u16>>],
) -> Vec<u16> {
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
            next if next == dollar => {
                output.push(dollar);
                index += 2;
            }
            next if next == u16::from(b'&') => {
                output.extend_from_slice(matched);
                index += 2;
            }
            next if next == u16::from(b'`') => {
                output.extend_from_slice(&subject[..position]);
                index += 2;
            }
            next if next == u16::from(b'\'') => {
                let end = (position + matched.len()).min(subject.len());
                output.extend_from_slice(&subject[end..]);
                index += 2;
            }
            next if (u16::from(b'0')..=u16::from(b'9')).contains(&next) => {
                let first = usize::from(next - u16::from(b'0'));
                let second = template
                    .get(index + 2)
                    .filter(|unit| (u16::from(b'0')..=u16::from(b'9')).contains(unit))
                    .map(|unit| usize::from(unit - u16::from(b'0')));
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
    output
}

fn argument(arguments: &[Value], index: usize) -> Value {
    arguments.get(index).cloned().unwrap_or(Value::Undefined)
}

impl ScriptRuntime {
    fn this_string(&mut self, this: &Value, method: StringMethod) -> Result<String, ScriptError> {
        match this {
            Value::String(text) => Ok(text.to_string()),
            Value::Undefined | Value::Null => Err(self.type_error(format!(
                "String.prototype.{method:?} called on null or undefined"
            ))),
            other => self.coerce_string(other.clone()),
        }
    }

    fn string_integer(&mut self, value: Value, default: f64) -> Result<f64, ScriptError> {
        if matches!(value, Value::Undefined) {
            return Ok(default);
        }
        let number = self.coerce_number(value)?;
        Ok(if number.is_nan() { 0.0 } else { number.trunc() })
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
        let mut expanded = expand_replacement(&template, matched, position, subject, captures);
        if let Value::Object(_) = groups {
            expanded = self.expand_named_groups(&expanded, &groups)?;
        }
        Ok(expanded)
    }

    // $<name> references to named groups in a replacement template
    fn expand_named_groups(
        &mut self,
        template: &[u16],
        groups: &Value,
    ) -> Result<Vec<u16>, ScriptError> {
        let mut output = Vec::with_capacity(template.len());
        let mut index = 0;
        while index < template.len() {
            if template[index] == u16::from(b'$')
                && template.get(index + 1) == Some(&u16::from(b'<'))
                && let Some(close) = template[index + 2..]
                    .iter()
                    .position(|unit| *unit == u16::from(b'>'))
            {
                let name = from_units(&template[index + 2..index + 2 + close]);
                let value = self.get_property(groups, &name)?;
                if !matches!(value, Value::Undefined) {
                    output.extend(units(&self.coerce_string(value)?));
                }
                index += close + 3;
                continue;
            }
            output.push(template[index]);
            index += 1;
        }
        Ok(output)
    }

    pub(super) fn call_string_method(
        &mut self,
        method: StringMethod,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use StringMethod as M;
        // regular-expression aware methods hand an object pattern its own protocol first
        if let M::Match | M::MatchAll | M::Replace | M::ReplaceAll | M::Search | M::Split = method {
            let pattern = argument(&arguments, 0);
            if !matches!(pattern, Value::Undefined | Value::Null | Value::String(_))
                && !super::operations::is_primitive(&pattern)
            {
                let symbol = match method {
                    M::Match => "@@match",
                    M::MatchAll => "@@matchAll",
                    M::Replace | M::ReplaceAll => "@@replace",
                    M::Search => "@@search",
                    _ => "@@split",
                };
                let protocol = self.get_property(&pattern, symbol)?;
                if is_callable(&protocol) {
                    if let M::ReplaceAll | M::MatchAll = method {
                        let flags = self.get_property(&pattern, "flags")?;
                        let flags = self.coerce_string(flags)?;
                        if !flags.contains('g') {
                            return Err(self.type_error(format!(
                                "{method:?} must be called with a global RegExp"
                            )));
                        }
                    }
                    let subject = self.this_string(&this, method)?;
                    let mut forwarded = vec![Value::String(subject.into())];
                    forwarded.extend(arguments.into_iter().skip(1));
                    return self.call_function(protocol, pattern, forwarded);
                }
            }
        }
        let text = self.this_string(&this, method)?;
        let first = argument(&arguments, 0);
        Ok(match method {
            M::At | M::CharAt | M::CharCodeAt | M::CodePointAt => {
                let length = utf16_length(&text);
                let index = self.string_integer(first, 0.0)?;
                let index = if matches!(method, M::At) && index < 0.0 {
                    length as f64 + index
                } else {
                    index
                };
                if index < 0.0 || index >= length as f64 {
                    return Ok(match method {
                        M::CharAt => Value::String("".into()),
                        M::CharCodeAt => Value::Number(f64::NAN),
                        _ => Value::Undefined,
                    });
                }
                let index = index as usize;
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
                let mut result = text;
                for value in arguments {
                    result.push_str(&self.coerce_string(value)?);
                }
                Value::String(result.into())
            }
            M::Includes | M::StartsWith | M::EndsWith | M::IndexOf | M::LastIndexOf => {
                if let M::Includes | M::StartsWith | M::EndsWith = method
                    && matches!(first, Value::Object(_))
                    && self.is_regexp(&first)?
                {
                    return Err(self.type_error(format!(
                        "First argument to String.prototype.{method:?} must not be a regular expression"
                    )));
                }
                let search = units(&self.coerce_string(first)?);
                let haystack = units(&text);
                let length = haystack.len();
                match method {
                    M::Includes | M::IndexOf => {
                        let from = self.string_integer(argument(&arguments, 1), 0.0)?;
                        let from = from.clamp(0.0, length as f64) as usize;
                        let found = find_units(&haystack, &search, from);
                        if matches!(method, M::Includes) {
                            Value::Bool(found.is_some())
                        } else {
                            Value::Number(found.map_or(-1.0, |index| index as f64))
                        }
                    }
                    M::LastIndexOf => {
                        let position = argument(&arguments, 1);
                        let from = match position {
                            Value::Undefined => length as f64,
                            position => {
                                let number = self.coerce_number(position)?;
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
                        let from = self.string_integer(argument(&arguments, 1), 0.0)?;
                        let start = from.clamp(0.0, length as f64) as usize;
                        Value::Bool(haystack[start..].starts_with(&search))
                    }
                    _ => {
                        let end = self.string_integer(argument(&arguments, 1), length as f64)?;
                        let end = end.clamp(0.0, length as f64) as usize;
                        Value::Bool(haystack[..end].ends_with(&search))
                    }
                }
            }
            M::Normalize => {
                let form = match first {
                    Value::Undefined => "NFC".to_string(),
                    form => self.coerce_string(form)?,
                };
                let Some(form) = crate::unicode::NormalizationForm::parse(&form) else {
                    return Err(self.range_error(
                        "The normalization form should be one of NFC, NFD, NFKC, NFKD.",
                    ));
                };
                Value::String(crate::unicode::normalize(&text, form).into())
            }
            M::IsWellFormed => Value::Bool(true),
            M::ToWellFormed => Value::String(text.into()),
            M::LocaleCompare => {
                let other = self.coerce_string(first)?;
                let order = locale_compare(&text, &other);
                Value::Number(match order {
                    std::cmp::Ordering::Less => -1.0,
                    std::cmp::Ordering::Equal => 0.0,
                    std::cmp::Ordering::Greater => 1.0,
                })
            }
            M::PadStart | M::PadEnd => {
                let target = self.string_integer(first, 0.0)?;
                let length = utf16_length(&text);
                let filler = match argument(&arguments, 1) {
                    Value::Undefined => vec![u16::from(b' ')],
                    filler => units(&self.coerce_string(filler)?),
                };
                if target <= length as f64 || filler.is_empty() {
                    return Ok(Value::String(text.into()));
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
                let count = self.string_integer(first, 0.0)?;
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
                let search = units(&self.coerce_string(first)?);
                let replacement = argument(&arguments, 1);
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
                    return Ok(Value::String(text.into()));
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
                let pattern = match first {
                    Value::Undefined => String::new(),
                    other => self.coerce_string(other)?,
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
                self.call_function(protocol, regexp, vec![Value::String(text.into())])?
            }
            M::Slice | M::Substring | M::Substr => {
                let length = utf16_length(&text) as f64;
                let start = self.string_integer(first, 0.0)?;
                let (start, end) = match method {
                    M::Slice => {
                        let end = self.string_integer(argument(&arguments, 1), length)?;
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
                        let end = self.string_integer(argument(&arguments, 1), length)?;
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
                        let count = self.string_integer(argument(&arguments, 1), length)?;
                        let end = (start + count.max(0.0)).min(length);
                        (start, end)
                    }
                };
                if start >= end {
                    return Ok(Value::String("".into()));
                }
                Value::String(utf16_slice(&text, start as usize, end as usize).into())
            }
            M::Split => {
                let limit = match argument(&arguments, 1) {
                    Value::Undefined => u32::MAX as usize,
                    limit => self.coerce_uint32(limit)? as usize,
                };
                if limit == 0 {
                    return Ok(Self::new_array(Vec::new()));
                }
                let parts: Vec<Value> = match first {
                    Value::Undefined => vec![Value::String(text.into())],
                    separator => {
                        let separator = units(&self.coerce_string(separator)?);
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
                };
                if parts.len() > MAX_RUNTIME_ARRAY_LENGTH {
                    return Err(
                        self.range_error("string split exceeds the deterministic array limit")
                    );
                }
                Self::new_array(parts)
            }
            M::ToLowerCase => Value::String(text.to_lowercase().into()),
            M::ToUpperCase => Value::String(text.to_uppercase().into()),
            M::Trim => Value::String(text.trim_matches(is_js_whitespace).into()),
            M::TrimStart => Value::String(text.trim_start_matches(is_js_whitespace).into()),
            M::TrimEnd => Value::String(text.trim_end_matches(is_js_whitespace).into()),
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
    let fold = |text: &str| {
        text.chars()
            .flat_map(char::to_lowercase)
            .collect::<Vec<char>>()
    };
    fold(left).cmp(&fold(right)).then_with(|| {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_templates_expand_like_the_spec() {
        let subject = units("abc");
        let output = expand_replacement(
            &units("[$$|$&|$`|$'|$1|$9]"),
            &units("b"),
            1,
            &subject,
            &[Some(units("x"))],
        );
        assert_eq!(from_units(&output), "[$|b|a|c|x|$9]");
    }

    #[test]
    fn utf16_offsets_cover_astral_characters() {
        assert_eq!(utf16_length("a\u{1f600}b"), 4);
        assert_eq!(utf16_slice("a\u{1f600}b", 1, 3), "\u{1f600}");
        assert_eq!(utf16_slice("hello", 1, 3), "el");
    }
}
