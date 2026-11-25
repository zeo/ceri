// RegExp objects: construction with a compiled-pattern cache, exec with lastIndex and the
// d, g and y flags, and the Symbol.match, matchAll, replace, search and split protocols

use super::object::{JsObject, ObjectClass, Property, Slot, new_object};
use super::operations::is_callable;
use super::pattern::{Flags, RegExpData, Regex, escape_source};
use super::strings::{from_units, units};
use super::*;

const PATTERN_CACHE_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug)]
pub(super) enum RegExpMethod {
    Constructor,
    Exec,
    Test,
    ToString,
    Match,
    MatchAll,
    Replace,
    Search,
    Split,
    Source,
    Flags,
    Flag(char),
}

pub(super) const REGEXP_METHODS: &[(&str, RegExpMethod, u32)] = &[
    ("exec", RegExpMethod::Exec, 1),
    ("test", RegExpMethod::Test, 1),
    ("toString", RegExpMethod::ToString, 0),
    ("@@match", RegExpMethod::Match, 1),
    ("@@matchAll", RegExpMethod::MatchAll, 1),
    ("@@replace", RegExpMethod::Replace, 2),
    ("@@search", RegExpMethod::Search, 1),
    ("@@split", RegExpMethod::Split, 2),
];

pub(super) const REGEXP_GETTERS: &[(&str, RegExpMethod)] = &[
    ("source", RegExpMethod::Source),
    ("flags", RegExpMethod::Flags),
    ("global", RegExpMethod::Flag('g')),
    ("ignoreCase", RegExpMethod::Flag('i')),
    ("multiline", RegExpMethod::Flag('m')),
    ("dotAll", RegExpMethod::Flag('s')),
    ("unicode", RegExpMethod::Flag('u')),
    ("unicodeSets", RegExpMethod::Flag('v')),
    ("sticky", RegExpMethod::Flag('y')),
    ("hasIndices", RegExpMethod::Flag('d')),
];

struct Found {
    captures: Vec<Option<(usize, usize)>>,
}

fn argument(arguments: &[Value], index: usize) -> Value {
    arguments.get(index).cloned().unwrap_or(Value::Undefined)
}

// the next index after a match that consumed nothing, stepping over a surrogate pair
fn advance(subject: &[u16], index: usize, unicode: bool) -> usize {
    if unicode
        && index + 1 < subject.len()
        && (0xd800..0xdc00).contains(&subject[index])
        && (0xdc00..0xe000).contains(&subject[index + 1])
    {
        index + 2
    } else {
        index + 1
    }
}

impl ScriptRuntime {
    // compiles a pattern once per source and flags; literals in loops reuse the program
    fn compiled_pattern(&mut self, pattern: &str, flags: &str) -> Result<Rc<Regex>, ScriptError> {
        if let Some((_, regex)) = self
            .pattern_cache
            .get(pattern)
            .and_then(|compiled| compiled.iter().find(|(known, _)| known == flags))
        {
            return Ok(regex.clone());
        }
        let parsed =
            Flags::parse(flags).map_err(|message| self.throw_error("SyntaxError", message))?;
        let regex = Regex::new(pattern, parsed).map_err(|message| {
            self.throw_error(
                "SyntaxError",
                format!("Invalid regular expression: /{pattern}/{flags}: {message}"),
            )
        })?;
        let regex = Rc::new(regex);
        if self.pattern_cache.len() >= PATTERN_CACHE_LIMIT {
            self.pattern_cache.clear();
        }
        self.pattern_cache
            .entry(pattern.to_string())
            .or_default()
            .push((flags.to_string(), regex.clone()));
        Ok(regex)
    }

    pub(super) fn new_regexp(&mut self, pattern: &str, flags: &str) -> Result<Value, ScriptError> {
        let prototype = self.intrinsics.regexp_prototype.clone();
        self.regexp_with_prototype(pattern, flags, prototype)
    }

    fn regexp_with_prototype(
        &mut self,
        pattern: &str,
        flags: &str,
        prototype: object::ObjectHandle,
    ) -> Result<Value, ScriptError> {
        let regex = self.compiled_pattern(pattern, flags)?;
        fn last_index() -> Property {
            Property {
                slot: Slot::Data(Value::Number(0.0)),
                enumerable: false,
                writable: true,
                configurable: false,
            }
        }
        thread_local! {
            // keeps the shape of a fresh RegExp alive, so a literal run in a loop shares it
            // instead of growing one per object once the last RegExp of it is gone
            static TEMPLATE: JsObject = {
                let mut template = JsObject::with_prototype(None, ObjectClass::Ordinary);
                template.define("lastIndex".to_string(), last_index());
                template
            };
        }
        let mut object =
            TEMPLATE.with(|template| JsObject::with_shape_of(template, vec![last_index()]));
        object.set_prototype(Some(prototype));
        object.class = ObjectClass::RegExp(Rc::new(RegExpData {
            source: pattern.to_string(),
            regex,
        }));
        Ok(Value::Object(new_object(object)))
    }

    fn regexp_data(
        &mut self,
        value: &Value,
        method: RegExpMethod,
    ) -> Result<Rc<RegExpData>, ScriptError> {
        if let Value::Object(object) = value
            && let ObjectClass::RegExp(data) = &object.borrow().class
        {
            return Ok(data.clone());
        }
        Err(self.type_error(format!(
            "RegExp.prototype.{method:?} requires that 'this' be a RegExp object"
        )))
    }

    fn last_index(&mut self, regexp: &Value) -> Result<usize, ScriptError> {
        let value = self.get_property(regexp, "lastIndex")?;
        let number = self.coerce_number(value)?;
        Ok(if number.is_nan() || number <= 0.0 {
            0
        } else {
            number.min(9_007_199_254_740_991.0) as usize
        })
    }

    fn set_last_index(&mut self, regexp: &Value, index: usize) -> Result<(), ScriptError> {
        self.set_property(regexp, "lastIndex", Value::Number(index as f64), true)
    }

    // RegExpBuiltinExec without building the result array
    fn exec_raw(
        &mut self,
        regexp: &Value,
        data: &RegExpData,
        subject: &[u16],
    ) -> Result<Option<Found>, ScriptError> {
        let flags = data.regex.flags;
        let uses_last_index = flags.global || flags.sticky;
        let start = if uses_last_index {
            self.last_index(regexp)?
        } else {
            0
        };
        if start > subject.len() {
            if uses_last_index {
                self.set_last_index(regexp, 0)?;
            }
            return Ok(None);
        }
        let found = data
            .regex
            .find(subject, start, flags.sticky)
            .map_err(|message| self.range_error(message))?;
        match found {
            None => {
                if uses_last_index {
                    self.set_last_index(regexp, 0)?;
                }
                Ok(None)
            }
            Some(found) => {
                if uses_last_index {
                    let end = found.captures[0].map_or(start, |(_, end)| end);
                    self.set_last_index(regexp, end)?;
                }
                Ok(Some(Found {
                    captures: found.captures,
                }))
            }
        }
    }

    fn groups_object(
        &self,
        data: &RegExpData,
        subject: &[u16],
        captures: &[Option<(usize, usize)>],
    ) -> Value {
        if data.regex.names.is_empty() {
            return Value::Undefined;
        }
        let mut groups = JsObject::with_prototype(None, ObjectClass::Ordinary);
        for (name, index) in &data.regex.names {
            let value = captures[*index]
                .map(|(start, end)| Value::String(from_units(&subject[start..end]).into()))
                .unwrap_or(Value::Undefined);
            groups.define(name.clone(), Property::data(value));
        }
        Value::Object(new_object(groups))
    }

    // the array exec returns, with index, input, groups and, for the d flag, indices
    fn match_array(
        &mut self,
        data: &RegExpData,
        subject: &[u16],
        text: &str,
        found: &Found,
    ) -> Value {
        let values: Vec<Value> = found
            .captures
            .iter()
            .map(|capture| match capture {
                Some((start, end)) => Value::String(from_units(&subject[*start..*end]).into()),
                None => Value::Undefined,
            })
            .collect();
        let array: ArrayHandle = super::heap::alloc_array(values.into());
        let start = found.captures[0].map_or(0, |(start, _)| start);
        self.set_array_expando(&array, "index", Value::Number(start as f64));
        self.set_array_expando(&array, "input", Value::String(text.into()));
        let groups = self.groups_object(data, subject, &found.captures);
        self.set_array_expando(&array, "groups", groups);
        if data.regex.flags.has_indices {
            let pairs: Vec<Value> = found
                .captures
                .iter()
                .map(|capture| match capture {
                    Some((start, end)) => Self::new_array(vec![
                        Value::Number(*start as f64),
                        Value::Number(*end as f64),
                    ]),
                    None => Value::Undefined,
                })
                .collect();
            let indices: ArrayHandle = super::heap::alloc_array(pairs.into());
            let named = if data.regex.names.is_empty() {
                Value::Undefined
            } else {
                let mut groups = JsObject::with_prototype(None, ObjectClass::Ordinary);
                for (name, index) in &data.regex.names {
                    let value = indices.borrow()[*index].clone();
                    groups.define(name.clone(), Property::data(value));
                }
                Value::Object(new_object(groups))
            };
            self.set_array_expando(&indices, "groups", named);
            self.set_array_expando(&array, "indices", Value::Array(indices));
        }
        Value::Array(array)
    }

    pub(super) fn call_regexp(
        &mut self,
        method: RegExpMethod,
        this: Value,
        arguments: Vec<Value>,
        new_target: Option<Value>,
    ) -> Result<Value, ScriptError> {
        use RegExpMethod as M;
        match method {
            M::Constructor => {
                let pattern = argument(&arguments, 0);
                let flags = argument(&arguments, 1);
                let existing = match &pattern {
                    Value::Object(object) => match &object.borrow().class {
                        ObjectClass::RegExp(data) => Some(data.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                if new_target.is_none() && existing.is_some() && matches!(flags, Value::Undefined) {
                    return Ok(pattern);
                }
                let source = match (&existing, &pattern) {
                    (Some(data), _) => data.source.clone(),
                    (None, Value::Undefined) => String::new(),
                    (None, other) => self.coerce_string(other.clone())?,
                };
                let flags = match (&existing, flags) {
                    (Some(data), Value::Undefined) => data.regex.flags.text(),
                    (_, Value::Undefined) => String::new(),
                    (_, flags) => self.coerce_string(flags)?,
                };
                let prototype = match &new_target {
                    Some(target @ Value::Object(_)) => self.prototype_from_constructor(target)?,
                    _ => self.intrinsics.regexp_prototype.clone(),
                };
                self.regexp_with_prototype(&source, &flags, prototype)
            }
            M::Source => match &this {
                Value::Object(object)
                    if ObjectHandle::ptr_eq(object, &self.intrinsics.regexp_prototype) =>
                {
                    Ok(Value::String("(?:)".into()))
                }
                _ => {
                    let data = self.regexp_data(&this, method)?;
                    Ok(Value::String(escape_source(&data.source).into()))
                }
            },
            M::Flags => {
                if super::operations::is_primitive(&this) {
                    return Err(
                        self.type_error("RegExp.prototype.flags getter called on non-object")
                    );
                }
                let mut text = String::new();
                for (name, letter) in [
                    ("hasIndices", 'd'),
                    ("global", 'g'),
                    ("ignoreCase", 'i'),
                    ("multiline", 'm'),
                    ("dotAll", 's'),
                    ("unicode", 'u'),
                    ("unicodeSets", 'v'),
                    ("sticky", 'y'),
                ] {
                    if self.get_property(&this, name)?.truthy() {
                        text.push(letter);
                    }
                }
                Ok(Value::String(text.into()))
            }
            M::Flag(letter) => match &this {
                Value::Object(object)
                    if ObjectHandle::ptr_eq(object, &self.intrinsics.regexp_prototype) =>
                {
                    Ok(Value::Undefined)
                }
                _ => {
                    let data = self.regexp_data(&this, method)?;
                    Ok(Value::Bool(data.regex.flags.text().contains(letter)))
                }
            },
            M::ToString => {
                if super::operations::is_primitive(&this) {
                    return Err(self.type_error("RegExp.prototype.toString called on non-object"));
                }
                let source = self.get_property(&this, "source")?;
                let source = self.coerce_string(source)?;
                let flags = self.get_property(&this, "flags")?;
                let flags = self.coerce_string(flags)?;
                Ok(Value::String(format!("/{source}/{flags}").into()))
            }
            M::Exec | M::Test => {
                let data = self.regexp_data(&this, method)?;
                let text = self.coerce_string(argument(&arguments, 0))?;
                let subject = units(&text);
                let found = self.exec_raw(&this, &data, &subject)?;
                Ok(match (method, found) {
                    (M::Test, found) => Value::Bool(found.is_some()),
                    (_, Some(found)) => self.match_array(&data, &subject, &text, &found),
                    (_, None) => Value::Null,
                })
            }
            M::Search => {
                let data = self.regexp_data(&this, method)?;
                let text = self.coerce_string(argument(&arguments, 0))?;
                let subject = units(&text);
                let found = data
                    .regex
                    .find(&subject, 0, data.regex.flags.sticky)
                    .map_err(|message| self.range_error(message))?;
                Ok(Value::Number(
                    found
                        .and_then(|found| found.captures[0])
                        .map_or(-1.0, |(start, _)| start as f64),
                ))
            }
            M::Match | M::MatchAll => {
                let data = self.regexp_data(&this, method)?;
                let text = self.coerce_string(argument(&arguments, 0))?;
                let subject = units(&text);
                let flags = data.regex.flags;
                if !flags.global && matches!(method, M::Match) {
                    let found = self.exec_raw(&this, &data, &subject)?;
                    return Ok(match found {
                        Some(found) => self.match_array(&data, &subject, &text, &found),
                        None => Value::Null,
                    });
                }
                // matchAll runs on a copy so the receiver's lastIndex is left alone
                let receiver = if matches!(method, M::MatchAll) {
                    let start = self.last_index(&this)?;
                    let copy = self.regexp_with_prototype(
                        &data.source,
                        &flags.text(),
                        self.intrinsics.regexp_prototype.clone(),
                    )?;
                    self.set_last_index(&copy, start)?;
                    copy
                } else {
                    self.set_last_index(&this, 0)?;
                    this.clone()
                };
                let mut results = Vec::new();
                while let Some(found) = self.exec_raw(&receiver, &data, &subject)? {
                    let (start, end) = found.captures[0].unwrap_or((0, 0));
                    if matches!(method, M::Match) {
                        results.push(Value::String(from_units(&subject[start..end]).into()));
                    } else {
                        results.push(self.match_array(&data, &subject, &text, &found));
                    }
                    if !flags.global {
                        break;
                    }
                    if start == end {
                        let next = advance(
                            &subject,
                            self.last_index(&receiver)?,
                            flags.unicode || flags.unicode_sets,
                        );
                        self.set_last_index(&receiver, next)?;
                    }
                    if results.len() > MAX_RUNTIME_ARRAY_LENGTH {
                        return Err(
                            self.range_error("match results exceed the deterministic limit")
                        );
                    }
                }
                Ok(match method {
                    M::Match if results.is_empty() => Value::Null,
                    M::Match => Self::new_array(results),
                    _ => Value::Iterator(Rc::new(RefCell::new(IteratorData {
                        values: results,
                        index: 0,
                    }))),
                })
            }
            M::Replace => {
                let data = self.regexp_data(&this, method)?;
                let text = self.coerce_string(argument(&arguments, 0))?;
                let subject = units(&text);
                let replacement = argument(&arguments, 1);
                let replacement = if is_callable(&replacement) {
                    replacement
                } else {
                    Value::String(self.coerce_string(replacement)?.into())
                };
                let flags = data.regex.flags;
                if flags.global {
                    self.set_last_index(&this, 0)?;
                }
                let mut matches = Vec::new();
                while let Some(found) = self.exec_raw(&this, &data, &subject)? {
                    let (start, end) = found.captures[0].unwrap_or((0, 0));
                    matches.push(found);
                    if !flags.global {
                        break;
                    }
                    if start == end {
                        let next = advance(
                            &subject,
                            self.last_index(&this)?,
                            flags.unicode || flags.unicode_sets,
                        );
                        self.set_last_index(&this, next)?;
                    }
                }
                let mut output = Vec::with_capacity(subject.len());
                let mut next = 0;
                for found in matches {
                    let (start, end) = found.captures[0].unwrap_or((0, 0));
                    let captures: Vec<Option<Vec<u16>>> = found.captures[1..]
                        .iter()
                        .map(|capture| capture.map(|(start, end)| subject[start..end].to_vec()))
                        .collect();
                    let groups = self.groups_object(&data, &subject, &found.captures);
                    let replaced = self.template_replacement(
                        &replacement,
                        &subject[start..end],
                        start,
                        &subject,
                        &captures,
                        groups,
                    )?;
                    if start >= next {
                        output.extend_from_slice(&subject[next..start]);
                        output.extend(replaced);
                        next = end;
                    }
                }
                output.extend_from_slice(&subject[next.min(subject.len())..]);
                Ok(Value::String(from_units(&output).into()))
            }
            M::Split => {
                let data = self.regexp_data(&this, method)?;
                let text = self.coerce_string(argument(&arguments, 0))?;
                let subject = units(&text);
                let limit = match argument(&arguments, 1) {
                    Value::Undefined => u32::MAX as usize,
                    limit => self.coerce_uint32(limit)? as usize,
                };
                let mut parts = Vec::new();
                if limit == 0 {
                    return Ok(Self::new_array(parts));
                }
                let unicode = data.regex.flags.unicode || data.regex.flags.unicode_sets;
                if subject.is_empty() {
                    let found = data
                        .regex
                        .find(&subject, 0, true)
                        .map_err(|message| self.range_error(message))?;
                    if found.is_none() {
                        parts.push(Value::String(text.into()));
                    }
                    return Ok(Self::new_array(parts));
                }
                // a pattern of one character or class splits at each unit it matches, with no
                // match run for each position
                if let Some(ascii) = data.regex.ascii_units() {
                    let mut previous = 0;
                    for (position, &unit) in subject.iter().enumerate() {
                        let found = match unit < 128 {
                            true => ascii >> unit & 1 == 1,
                            false => data.regex.unit_match(unit) == Some(true),
                        };
                        if found {
                            parts.push(Value::String(
                                from_units(&subject[previous..position]).into(),
                            ));
                            if parts.len() == limit {
                                return Ok(Self::new_array(parts));
                            }
                            previous = position + 1;
                        }
                    }
                    parts.push(Value::String(from_units(&subject[previous..]).into()));
                    return Ok(Self::new_array(parts));
                }
                let mut previous = 0;
                let mut position = 0;
                while position < subject.len() {
                    let found = data
                        .regex
                        .find(&subject, position, true)
                        .map_err(|message| self.range_error(message))?;
                    let Some(found) = found else {
                        position = advance(&subject, position, unicode);
                        continue;
                    };
                    let end = found.captures[0]
                        .map_or(position, |(_, end)| end)
                        .min(subject.len());
                    if end == previous {
                        position = advance(&subject, position, unicode);
                        continue;
                    }
                    parts.push(Value::String(
                        from_units(&subject[previous..position]).into(),
                    ));
                    if parts.len() == limit {
                        return Ok(Self::new_array(parts));
                    }
                    for capture in &found.captures[1..] {
                        parts.push(match capture {
                            Some((start, end)) => {
                                Value::String(from_units(&subject[*start..*end]).into())
                            }
                            None => Value::Undefined,
                        });
                        if parts.len() == limit {
                            return Ok(Self::new_array(parts));
                        }
                    }
                    previous = end;
                    position = end;
                }
                parts.push(Value::String(from_units(&subject[previous..]).into()));
                Ok(Self::new_array(parts))
            }
        }
    }
}
