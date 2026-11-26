// RegExp objects: construction with a compiled-pattern cache, exec with lastIndex and the
// d, g and y flags, and the Symbol.match, matchAll, replace, search and split protocols

use super::object::{JsObject, ObjectClass, Property, Slot, new_object};
use super::operations::is_callable;
use super::pattern::{Flags, RegExpData, Regex, escape_source};
use super::strings::{from_units, units};
use super::*;

const PATTERN_CACHE_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RegExpMethod {
    Constructor,
    Species,
    Exec,
    Test,
    ToString,
    Match,
    MatchAll,
    MatchAllNext,
    Replace,
    Search,
    Split,
    Source,
    Flags,
    Flag(char),
}

pub(super) struct RegExpIteratorState {
    pub(super) matcher: Value,
    pub(super) string: JsStr,
    pub(super) global: bool,
    pub(super) full_unicode: bool,
    pub(super) done: bool,
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
        object.set_class(ObjectClass::RegExp(Rc::new(RegExpData {
            source: pattern.to_string(),
            regex,
        })));
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
        if let Value::Object(obj) = regexp
            && let Some(number) = obj.borrow().plain_writable_last_index()
        {
            return Ok(if number.is_nan() || number <= 0.0 {
                0
            } else {
                number.min(9_007_199_254_740_991.0) as usize
            });
        }
        let value = self.get_property(regexp, "lastIndex")?;
        let number = self.coerce_number(value)?;
        Ok(if number.is_nan() || number <= 0.0 {
            0
        } else {
            number.min(9_007_199_254_740_991.0) as usize
        })
    }

    fn set_last_index(&mut self, regexp: &Value, index: usize) -> Result<(), ScriptError> {
        if let Value::Object(obj) = regexp
            && obj.borrow_mut().set_plain_last_index(index)
        {
            return Ok(());
        }
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
            // read lastindex unconditionally per spec unless it is a plain own number
            let plain_num = matches!(
                regexp,
                Value::Object(obj) if obj.borrow().plain_writable_last_index().is_some()
                    || obj.borrow().property("lastIndex").is_some_and(|p| matches!(p.slot, Slot::Data(Value::Number(_))))
            );
            if !plain_num {
                let _ = self.last_index(regexp)?;
            }
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

    #[inline]
    fn slot_is_regexp_method(
        heap: &super::heap::Heap,
        slot: &Slot,
        expected: RegExpMethod,
    ) -> bool {
        let h = match slot {
            Slot::Data(Value::Object(h)) => h,
            Slot::Accessor(acc) => match &acc.get {
                Some(Value::Object(h)) => h,
                _ => return false,
            },
            _ => return false,
        };
        unsafe { heap.object_ref(h) }.is_some_and(|obj| {
            matches!(
                &obj.class,
                ObjectClass::Native(native) if matches!(**native, Native::RegExp(m) if m == expected)
            )
        })
    }

    #[inline]
    pub(super) fn plain_regexp_data(
        &self,
        heap: &super::heap::Heap,
        value: &Value,
    ) -> Option<Rc<RegExpData>> {
        let Value::Object(obj) = value else {
            return None;
        };
        let borrowed = unsafe { heap.object_ref(obj)? };
        let ObjectClass::RegExp(data) = &borrowed.class else {
            return None;
        };
        if !borrowed.is_plain_regexp_own() {
            return None;
        }
        borrowed
            .own_prototype()
            .is_some_and(|p| ObjectHandle::ptr_eq(p, &self.intrinsics.regexp_prototype))
            .then(|| data.clone())
    }

    #[inline]
    pub(super) fn regexp_proto_slot_intact(
        &self,
        heap: &super::heap::Heap,
        slot_index: usize,
        expected: RegExpMethod,
    ) -> bool {
        let Some(proto) = (unsafe { heap.object_ref(&self.intrinsics.regexp_prototype) }) else {
            return false;
        };
        proto.shape_id() == self.intrinsics.regexp_proto_shape
            && proto
                .slots()
                .get(slot_index)
                .is_some_and(|s| Self::slot_is_regexp_method(heap, s, expected))
    }

    #[inline]
    fn regexp_flags_and_exec_intact(&self, heap: &super::heap::Heap) -> bool {
        let Some(proto) = (unsafe { heap.object_ref(&self.intrinsics.regexp_prototype) }) else {
            return false;
        };
        if proto.shape_id() != self.intrinsics.regexp_proto_shape {
            return false;
        }
        let slots = proto.slots();
        if slots.len() <= 17 {
            return false;
        }
        Self::slot_is_regexp_method(heap, &slots[0], RegExpMethod::Exec)
            && Self::slot_is_regexp_method(heap, &slots[9], RegExpMethod::Flags)
            && Self::slot_is_regexp_method(heap, &slots[10], RegExpMethod::Flag('g'))
            && Self::slot_is_regexp_method(heap, &slots[11], RegExpMethod::Flag('i'))
            && Self::slot_is_regexp_method(heap, &slots[12], RegExpMethod::Flag('m'))
            && Self::slot_is_regexp_method(heap, &slots[13], RegExpMethod::Flag('s'))
            && Self::slot_is_regexp_method(heap, &slots[14], RegExpMethod::Flag('u'))
            && Self::slot_is_regexp_method(heap, &slots[15], RegExpMethod::Flag('v'))
            && Self::slot_is_regexp_method(heap, &slots[16], RegExpMethod::Flag('y'))
            && Self::slot_is_regexp_method(heap, &slots[17], RegExpMethod::Flag('d'))
    }

    pub(super) fn regexp_species_and_exec_intact(&self, heap: &super::heap::Heap) -> bool {
        if !self.regexp_flags_and_exec_intact(heap) {
            return false;
        }
        let Some(proto) = (unsafe { heap.object_ref(&self.intrinsics.regexp_prototype) }) else {
            return false;
        };
        let Some(Slot::Data(Value::Object(ctor_h))) = proto.slots().get(18) else {
            return false;
        };
        if !ObjectHandle::ptr_eq(ctor_h, &self.intrinsics.regexp_constructor) {
            return false;
        }
        let Some(ctor) = (unsafe { heap.object_ref(&self.intrinsics.regexp_constructor) }) else {
            return false;
        };
        if ctor.shape_id() == self.intrinsics.regexp_ctor_shape
            && let Some(slot) = ctor.slots().get(3)
        {
            return Self::slot_is_regexp_method(heap, slot, RegExpMethod::Species);
        }
        ctor.property("@@species")
            .is_some_and(|p| Self::slot_is_regexp_method(heap, p.slot, RegExpMethod::Species))
    }

    fn regexp_species(&mut self, rx: &Value) -> Result<Value, ScriptError> {
        let intrinsic = Value::Object(self.intrinsics.regexp_constructor.clone());
        let constructor = self.get_property(rx, "constructor")?;
        if matches!(constructor, Value::Undefined) {
            return Ok(intrinsic);
        }
        if super::operations::is_primitive(&constructor) {
            return Err(self.type_error("RegExp constructor is not an object"));
        }
        let species = self.get_property(&constructor, "@@species")?;
        if matches!(species, Value::Undefined | Value::Null) {
            return Ok(intrinsic);
        }
        if !Self::is_constructor(&species) {
            return Err(self.type_error("RegExp species is not a constructor"));
        }
        Ok(species)
    }

    fn regexp_exec(&mut self, rx: &Value, text: &JsStr) -> Result<Value, ScriptError> {
        let exec = self.get_property(rx, "exec")?;
        if is_callable(&exec) {
            let result = self.call_function(exec, rx.clone(), vec![Value::String(text.clone())])?;
            if !matches!(result, Value::Null) && super::operations::is_primitive(&result) {
                return Err(self.type_error(
                    "RegExp exec method returned something other than an Object or null",
                ));
            }
            return Ok(result);
        }
        let data = self.regexp_data(rx, RegExpMethod::Exec)?;
        let subject = units(text);
        let found = self.exec_raw(rx, &data, &subject)?;
        Ok(match found {
            Some(found) => self.match_array(&data, &subject, text, &found),
            None => Value::Null,
        })
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
        for (name, indices) in &data.regex.names {
            let value = indices
                .iter()
                .find_map(|&index| captures[index])
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
                let borrowed = indices.borrow();
                for (name, group_indices) in &data.regex.names {
                    let value = group_indices
                        .iter()
                        .find_map(|&idx| {
                            let val = &borrowed[idx];
                            (!matches!(val, Value::Undefined)).then(|| val.clone())
                        })
                        .unwrap_or(Value::Undefined);
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
            M::Species => Ok(this),
            M::Constructor => {
                let pattern = argument(&arguments, 0);
                let flags = argument(&arguments, 1);
                let pattern_is_regexp = self.is_regexp(&pattern)?;
                if new_target.is_none() && pattern_is_regexp && matches!(flags, Value::Undefined) {
                    let ctor = self.get_property(&pattern, "constructor")?;
                    if matches!(
                        ctor,
                        Value::Object(ref o)
                            if ObjectHandle::ptr_eq(o, &self.intrinsics.regexp_constructor)
                    ) {
                        return Ok(pattern);
                    }
                }
                let existing = match &pattern {
                    Value::Object(object) => match &object.borrow().class {
                        ObjectClass::RegExp(data) => Some(data.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let (source, flags_str) = if let Some(data) = existing {
                    let f = match flags {
                        Value::Undefined => data.regex.flags.text(),
                        other => self.coerce_string(other)?,
                    };
                    (data.source.clone(), f)
                } else if pattern_is_regexp {
                    let p = self.get_property(&pattern, "source")?;
                    let f = if matches!(flags, Value::Undefined) {
                        self.get_property(&pattern, "flags")?
                    } else {
                        flags
                    };
                    let s = match p {
                        Value::Undefined => String::new(),
                        other => self.coerce_string(other)?,
                    };
                    let fl = match f {
                        Value::Undefined => String::new(),
                        other => self.coerce_string(other)?,
                    };
                    (s, fl)
                } else {
                    let s = match pattern {
                        Value::Undefined => String::new(),
                        other => self.coerce_string(other)?,
                    };
                    let fl = match flags {
                        Value::Undefined => String::new(),
                        other => self.coerce_string(other)?,
                    };
                    (s, fl)
                };
                let prototype = match &new_target {
                    Some(target @ Value::Object(_)) => self.prototype_from_constructor(target)?,
                    _ => self.intrinsics.regexp_prototype.clone(),
                };
                self.regexp_with_prototype(&source, &flags_str, prototype)
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
            M::Exec => {
                let data = self.regexp_data(&this, method)?;
                let text = self.coerce_string(argument(&arguments, 0))?;
                let subject = units(&text);
                let found = self.exec_raw(&this, &data, &subject)?;
                Ok(match found {
                    Some(found) => self.match_array(&data, &subject, &text, &found),
                    None => Value::Null,
                })
            }
            M::Test => {
                if super::operations::is_primitive(&this) {
                    return Err(self.type_error("RegExp.prototype.test called on non-object"));
                }
                let text: JsStr = self.coerce_string(argument(&arguments, 0))?.into();
                let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
                if let Some(data) = self.plain_regexp_data(heap, &this)
                    && self.regexp_proto_slot_intact(heap, 0, RegExpMethod::Exec)
                {
                    let subject = units(&text);
                    let found = self.exec_raw(&this, &data, &subject)?;
                    return Ok(Value::Bool(found.is_some()));
                }
                let found = self.regexp_exec(&this, &text)?;
                Ok(Value::Bool(!matches!(found, Value::Null)))
            }
            M::Search => {
                if super::operations::is_primitive(&this) {
                    return Err(
                        self.type_error("RegExp.prototype[Symbol.search] called on non-object")
                    );
                }
                let text: JsStr = self.coerce_string(argument(&arguments, 0))?.into();
                let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
                if let Some(data) = self.plain_regexp_data(heap, &this)
                    && self.regexp_proto_slot_intact(heap, 0, RegExpMethod::Exec)
                    && let Value::Object(obj) = &this
                    && obj.borrow().plain_writable_last_index().is_some()
                {
                    let subject = units(&text);
                    let found = data
                        .regex
                        .find(&subject, 0, data.regex.flags.sticky)
                        .map_err(|message| self.range_error(message))?;
                    return Ok(Value::Number(
                        found.map_or(-1.0, |m| m.captures[0].map_or(0, |(s, _)| s) as f64),
                    ));
                }
                let previous = self.get_property(&this, "lastIndex")?;
                if !super::operations::same_value(&previous, &Value::Number(0.0)) {
                    self.set_last_index(&this, 0)?;
                }
                let result = self.regexp_exec(&this, &text)?;
                let current = self.get_property(&this, "lastIndex")?;
                if !super::operations::same_value(&current, &previous) {
                    self.set_property(&this, "lastIndex", previous, true)?;
                }
                if matches!(result, Value::Null) {
                    return Ok(Value::Number(-1.0));
                }
                self.get_property(&result, "index")
            }
            M::Match => {
                if super::operations::is_primitive(&this) {
                    return Err(
                        self.type_error("RegExp.prototype[Symbol.match] called on non-object")
                    );
                }
                let text: JsStr = self.coerce_string(argument(&arguments, 0))?.into();
                let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
                if let Some(data) = self.plain_regexp_data(heap, &this)
                    && self.regexp_flags_and_exec_intact(heap)
                {
                    let subject = units(&text);
                    if !data.regex.flags.global {
                        let found = self.exec_raw(&this, &data, &subject)?;
                        return Ok(match found {
                            Some(found) => self.match_array(&data, &subject, &text, &found),
                            None => Value::Null,
                        });
                    }
                    let full_unicode = data.regex.flags.unicode || data.regex.flags.unicode_sets;
                    self.set_last_index(&this, 0)?;
                    let mut results = Vec::new();
                    let mut pos = 0usize;
                    while pos <= subject.len() {
                        let found = data
                            .regex
                            .find(&subject, pos, data.regex.flags.sticky)
                            .map_err(|message| self.range_error(message))?;
                        let Some(found) = found else {
                            break;
                        };
                        let (start, end) = found.captures[0].unwrap_or((pos, pos));
                        results.push(Value::String(from_units(&subject[start..end]).into()));
                        if results.len() > MAX_RUNTIME_ARRAY_LENGTH {
                            return Err(
                                self.range_error("match results exceed the deterministic limit")
                            );
                        }
                        pos = if start == end {
                            advance(&subject, end, full_unicode)
                        } else {
                            end
                        };
                    }
                    self.set_last_index(&this, 0)?;
                    return Ok(if results.is_empty() {
                        Value::Null
                    } else {
                        Self::new_array(results)
                    });
                }
                let flags_val = self.get_property(&this, "flags")?;
                let flags_str = self.coerce_string(flags_val)?;
                if !flags_str.contains('g') {
                    return self.regexp_exec(&this, &text);
                }
                let full_unicode = flags_str.contains('u') || flags_str.contains('v');
                self.set_last_index(&this, 0)?;
                let subject = units(&text);
                let mut results = Vec::new();
                loop {
                    let result = self.regexp_exec(&this, &text)?;
                    if matches!(result, Value::Null) {
                        return Ok(if results.is_empty() {
                            Value::Null
                        } else {
                            Self::new_array(results)
                        });
                    }
                    let matched = self.get_property(&result, "0")?;
                    let matched_str = self.coerce_string(matched)?;
                    let empty = matched_str.is_empty();
                    results.push(Value::String(matched_str.into()));
                    if empty {
                        let this_index = self.last_index(&this)?;
                        let next = advance(&subject, this_index, full_unicode);
                        self.set_last_index(&this, next)?;
                    }
                    if results.len() > MAX_RUNTIME_ARRAY_LENGTH {
                        return Err(
                            self.range_error("match results exceed the deterministic limit")
                        );
                    }
                }
            }
            M::MatchAll => {
                if super::operations::is_primitive(&this) {
                    return Err(
                        self.type_error("RegExp.prototype[Symbol.matchAll] called on non-object")
                    );
                }
                let text: JsStr = self.coerce_string(argument(&arguments, 0))?.into();
                let ctor = self.regexp_species(&this)?;
                let flags_val = self.get_property(&this, "flags")?;
                let flags_str = self.coerce_string(flags_val)?;
                let matcher = self.construct(
                    ctor,
                    vec![this.clone(), Value::String(flags_str.clone().into())],
                    None,
                )?;
                let last_index = self.last_index(&this)?;
                self.set_last_index(&matcher, last_index)?;
                let global = flags_str.contains('g');
                let full_unicode = flags_str.contains('u') || flags_str.contains('v');
                let state = Rc::new(RefCell::new(RegExpIteratorState {
                    matcher,
                    string: text,
                    global,
                    full_unicode,
                    done: false,
                }));
                Ok(Value::Object(new_object(JsObject::with_prototype(
                    Some(self.intrinsics.regexp_string_iterator_prototype.clone()),
                    ObjectClass::RegExpIterator(state),
                ))))
            }
            M::MatchAllNext => {
                let state = match &this {
                    Value::Object(object) => match &object.borrow().class {
                        ObjectClass::RegExpIterator(state) => Some(state.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(state) = state else {
                    return Err(self.type_error(
                        "%RegExpStringIteratorPrototype%.next requires a RegExp String Iterator",
                    ));
                };
                if state.borrow().done {
                    return Ok(self.iterator_result(Value::Undefined, true));
                }
                let (matcher, text, global, full_unicode) = {
                    let s = state.borrow();
                    (
                        s.matcher.clone(),
                        s.string.clone(),
                        s.global,
                        s.full_unicode,
                    )
                };
                let found = self.regexp_exec(&matcher, &text)?;
                if matches!(found, Value::Null) {
                    state.borrow_mut().done = true;
                    return Ok(self.iterator_result(Value::Undefined, true));
                }
                if global {
                    let m0 = self.get_property(&found, "0")?;
                    let m0_str = self.coerce_string(m0)?;
                    if m0_str.is_empty() {
                        let this_index = self.last_index(&matcher)?;
                        let subject = units(&text);
                        let next = advance(&subject, this_index, full_unicode);
                        self.set_last_index(&matcher, next)?;
                    }
                } else {
                    state.borrow_mut().done = true;
                }
                Ok(self.iterator_result(found, false))
            }
            M::Replace => {
                if super::operations::is_primitive(&this) {
                    return Err(
                        self.type_error("RegExp.prototype[Symbol.replace] called on non-object")
                    );
                }
                let text: JsStr = self.coerce_string(argument(&arguments, 0))?.into();
                let replacement = argument(&arguments, 1);
                let callable = is_callable(&replacement);
                let replacement = if callable {
                    replacement
                } else {
                    Value::String(self.coerce_string(replacement)?.into())
                };
                let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
                if let Some(data) = self.plain_regexp_data(heap, &this)
                    && self.regexp_flags_and_exec_intact(heap)
                {
                    let subject = units(&text);
                    let flags = data.regex.flags;
                    let full_unicode = flags.unicode || flags.unicode_sets;
                    let mut found_list = Vec::new();
                    if flags.global {
                        self.set_last_index(&this, 0)?;
                        let mut pos = 0usize;
                        while pos <= subject.len() {
                            let found = data
                                .regex
                                .find(&subject, pos, flags.sticky)
                                .map_err(|message| self.range_error(message))?;
                            let Some(found) = found else {
                                break;
                            };
                            let (start, end) = found.captures[0].unwrap_or((pos, pos));
                            found_list.push(Found {
                                captures: found.captures,
                            });
                            if found_list.len() > MAX_RUNTIME_ARRAY_LENGTH {
                                return Err(self.range_error(
                                    "replace results exceed the deterministic limit",
                                ));
                            }
                            pos = if start == end {
                                advance(&subject, end, full_unicode)
                            } else {
                                end
                            };
                        }
                        self.set_last_index(&this, 0)?;
                    } else if let Some(found) = self.exec_raw(&this, &data, &subject)? {
                        found_list.push(found);
                    }
                    if found_list.is_empty() {
                        return Ok(Value::String(text));
                    }
                    let mut output = Vec::with_capacity(subject.len());
                    let mut next_source_pos = 0usize;
                    if !callable {
                        let Value::String(tpl_str) = &replacement else {
                            unreachable!()
                        };
                        let tpl = units(tpl_str);
                        if !tpl.contains(&u16::from(b'$')) {
                            for found in &found_list {
                                let (start, end) = found.captures[0].unwrap_or((0, 0));
                                if start >= next_source_pos {
                                    output.extend_from_slice(&subject[next_source_pos..start]);
                                    output.extend_from_slice(&tpl);
                                    next_source_pos = end;
                                }
                            }
                            output.extend_from_slice(&subject[next_source_pos..]);
                            return Ok(Value::String(from_units(&output).into()));
                        }
                        let has_named = !data.regex.names.is_empty();
                        for found in &found_list {
                            let (start, end) = found.captures[0].unwrap_or((0, 0));
                            let captures: Vec<Option<Vec<u16>>> = found
                                .captures
                                .iter()
                                .skip(1)
                                .map(|cap| cap.map(|(s, e)| subject[s..e].to_vec()))
                                .collect();
                            let groups = if has_named {
                                Some(self.groups_object(&data, &subject, &found.captures))
                            } else {
                                None
                            };
                            let replaced = self.expand_substitution(
                                &tpl,
                                &subject[start..end],
                                start,
                                &subject,
                                &captures,
                                groups.as_ref(),
                            )?;
                            if start >= next_source_pos {
                                output.extend_from_slice(&subject[next_source_pos..start]);
                                output.extend(replaced);
                                next_source_pos = end;
                            }
                        }
                        output.extend_from_slice(&subject[next_source_pos..]);
                        return Ok(Value::String(from_units(&output).into()));
                    }
                    let has_named = !data.regex.names.is_empty();
                    for found in &found_list {
                        let (start, end) = found.captures[0].unwrap_or((0, 0));
                        let mut call_args = Vec::with_capacity(found.captures.len() + 3);
                        call_args.push(Value::String(from_units(&subject[start..end]).into()));
                        for cap in found.captures.iter().skip(1) {
                            call_args.push(match cap {
                                Some((s, e)) => Value::String(from_units(&subject[*s..*e]).into()),
                                None => Value::Undefined,
                            });
                        }
                        call_args.push(Value::Number(start as f64));
                        call_args.push(Value::String(text.clone()));
                        if has_named {
                            call_args.push(self.groups_object(&data, &subject, &found.captures));
                        }
                        let result =
                            self.call_function(replacement.clone(), Value::Undefined, call_args)?;
                        let replaced = units(&self.coerce_string(result)?);
                        if start >= next_source_pos {
                            output.extend_from_slice(&subject[next_source_pos..start]);
                            output.extend(replaced);
                            next_source_pos = end;
                        }
                    }
                    output.extend_from_slice(&subject[next_source_pos..]);
                    return Ok(Value::String(from_units(&output).into()));
                }
                let subject = units(&text);
                let flags_val = self.get_property(&this, "flags")?;
                let flags_str = self.coerce_string(flags_val)?;
                let global = flags_str.contains('g');
                let full_unicode = flags_str.contains('u') || flags_str.contains('v');
                if global {
                    self.set_last_index(&this, 0)?;
                }
                let mut results = Vec::new();
                loop {
                    let result = self.regexp_exec(&this, &text)?;
                    if matches!(result, Value::Null) {
                        break;
                    }
                    results.push(result.clone());
                    if !global {
                        break;
                    }
                    let m0 = self.get_property(&result, "0")?;
                    let m0_str = self.coerce_string(m0)?;
                    if m0_str.is_empty() {
                        let this_index = self.last_index(&this)?;
                        let next = advance(&subject, this_index, full_unicode);
                        self.set_last_index(&this, next)?;
                    }
                    if results.len() > MAX_RUNTIME_ARRAY_LENGTH {
                        return Err(
                            self.range_error("replace results exceed the deterministic limit")
                        );
                    }
                }
                let mut output = Vec::with_capacity(subject.len());
                let mut next_source_pos = 0usize;
                for result in results {
                    let len_val = self.get_property(&result, "length")?;
                    let len_num = self.coerce_number(len_val)?;
                    let n_captures = if len_num.is_nan() || len_num <= 1.0 {
                        0
                    } else {
                        ((len_num.min(9_007_199_254_740_991.0) as usize) - 1)
                            .min(MAX_RUNTIME_ARRAY_LENGTH)
                    };
                    let m0 = self.get_property(&result, "0")?;
                    let matched_str = self.coerce_string(m0)?;
                    let matched = units(&matched_str);
                    let pos_val = self.get_property(&result, "index")?;
                    let pos_num = self.coerce_number(pos_val)?;
                    let position = if pos_num.is_nan() || pos_num <= 0.0 {
                        0
                    } else {
                        (pos_num.min(subject.len() as f64)) as usize
                    };
                    let mut captures = Vec::with_capacity(n_captures);
                    for i in 1..=n_captures {
                        let cap = self.get_property(&result, &i.to_string())?;
                        if matches!(cap, Value::Undefined) {
                            captures.push(None);
                        } else {
                            captures.push(Some(units(&self.coerce_string(cap)?)));
                        }
                    }
                    let groups = self.get_property(&result, "groups")?;
                    let replaced = self.template_replacement(
                        &replacement,
                        &matched,
                        position,
                        &subject,
                        &captures,
                        groups,
                    )?;
                    if position >= next_source_pos {
                        output.extend_from_slice(&subject[next_source_pos..position]);
                        output.extend(replaced);
                        next_source_pos = (position + matched.len()).min(subject.len());
                    }
                }
                output.extend_from_slice(&subject[next_source_pos.min(subject.len())..]);
                Ok(Value::String(from_units(&output).into()))
            }
            M::Split => {
                if super::operations::is_primitive(&this) {
                    return Err(
                        self.type_error("RegExp.prototype[Symbol.split] called on non-object")
                    );
                }
                let text: JsStr = match argument(&arguments, 0) {
                    Value::String(text) => text,
                    other => self.coerce_string(other)?.into(),
                };
                // fast path when receiver is a plain intrinsic regexp with untouched species, exec, and flags
                let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
                if let Some(data) = self.plain_regexp_data(heap, &this)
                    && self.regexp_species_and_exec_intact(heap)
                {
                    let limit = match argument(&arguments, 1) {
                        Value::Undefined => u32::MAX as usize,
                        limit => self.coerce_uint32(limit)? as usize,
                    };
                    return self.regexp_split_str_fast(heap, &data, &text, limit);
                }
                let ctor = self.regexp_species(&this)?;
                let flags_val = self.get_property(&this, "flags")?;
                let flags_str = self.coerce_string(flags_val)?;
                let unicode = flags_str.contains('u') || flags_str.contains('v');
                let new_flags = if flags_str.contains('y') {
                    flags_str
                } else {
                    format!("{flags_str}y")
                };
                let splitter = self.construct(
                    ctor,
                    vec![this.clone(), Value::String(new_flags.into())],
                    None,
                )?;
                let limit = match argument(&arguments, 1) {
                    Value::Undefined => u32::MAX as usize,
                    limit => self.coerce_uint32(limit)? as usize,
                };
                if limit == 0 {
                    return Ok(Self::new_array(Vec::new()));
                }
                let subject = units(&text);
                if subject.is_empty() {
                    let z = self.regexp_exec(&splitter, &text)?;
                    return Ok(if matches!(z, Value::Null) {
                        Self::new_array(vec![Value::String(text)])
                    } else {
                        Self::new_array(Vec::new())
                    });
                }
                let mut parts = Vec::new();
                let mut p = 0usize;
                let mut q = 0usize;
                while q < subject.len() {
                    self.set_last_index(&splitter, q)?;
                    let z = self.regexp_exec(&splitter, &text)?;
                    if matches!(z, Value::Null) {
                        q = advance(&subject, q, unicode);
                        continue;
                    }
                    let e = self.last_index(&splitter)?.min(subject.len());
                    if e == p {
                        q = advance(&subject, q, unicode);
                        continue;
                    }
                    parts.push(Value::String(from_units(&subject[p..q]).into()));
                    if parts.len() == limit {
                        return Ok(Self::new_array(parts));
                    }
                    p = e;
                    let len_val = self.get_property(&z, "length")?;
                    let len_num = self.coerce_number(len_val)?;
                    let n_captures = if len_num.is_nan() || len_num <= 1.0 {
                        0
                    } else {
                        ((len_num.min(9_007_199_254_740_991.0) as usize) - 1)
                            .min(MAX_RUNTIME_ARRAY_LENGTH)
                    };
                    for i in 1..=n_captures {
                        let cap = self.get_property(&z, &i.to_string())?;
                        parts.push(cap);
                        if parts.len() == limit {
                            return Ok(Self::new_array(parts));
                        }
                    }
                    q = p;
                }
                parts.push(Value::String(from_units(&subject[p..]).into()));
                Ok(Self::new_array(parts))
            }
        }
    }

    pub(super) fn regexp_split_str_fast(
        &mut self,
        heap: &super::heap::Heap,
        data: &RegExpData,
        text: &JsStr,
        limit: usize,
    ) -> Result<Value, ScriptError> {
        if limit == 0 {
            return Ok(Value::Array(heap.alloc_array_fast(ArrayData::default())));
        }
        // fast ASCII byte scan for single-unit/class and greedy `+` split patterns
        if let Some((split_kind, repeat)) = data.regex.ascii_split()
            && text.is_ascii()
        {
            if text.is_empty() {
                return Ok(Value::Array(heap.alloc_array_fast(ArrayData::from(vec![
                    Value::String(text.clone()),
                ]))));
            }
            let bytes = text.as_bytes();
            let parts = if !repeat {
                Self::split_ascii_single(text, bytes, split_kind, limit)
            } else {
                Self::split_ascii_repeat(text, bytes, split_kind, limit)
            };
            return Ok(Value::Array(heap.alloc_array_fast(ArrayData::from(parts))));
        }
        let mut parts = Vec::new();
        let subject = units(text);
        let unicode = data.regex.flags.unicode || data.regex.flags.unicode_sets;
        if subject.is_empty() {
            let found = data
                .regex
                .find(&subject, 0, true)
                .map_err(|message| self.range_error(message))?;
            if found.is_none() {
                parts.push(Value::String(text.clone()));
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
                    Some((start, end)) => Value::String(from_units(&subject[*start..*end]).into()),
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

    #[inline]
    fn split_ascii_single(
        text: &JsStr,
        bytes: &[u8],
        kind: super::pattern::AsciiSplit,
        limit: usize,
    ) -> Vec<Value> {
        use super::pattern::AsciiSplit;
        let mut cuts = [0usize; 32];
        let mut n = 0usize;
        let mut overflow_parts: Option<Vec<Value>> = None;
        let mut previous = 0usize;
        for (position, &byte) in bytes.iter().enumerate() {
            let hit = match kind {
                AsciiSplit::One(b0) => byte == b0,
                AsciiSplit::Two(b0, b1) => byte == b0 || byte == b1,
                AsciiSplit::Mask(ascii) => (ascii >> byte) & 1 == 1,
            };
            if !hit {
                continue;
            }
            if let Some(parts) = overflow_parts.as_mut() {
                parts.push(Value::String(JsStr::new(&text[previous..position])));
                if parts.len() == limit {
                    return overflow_parts.unwrap();
                }
                previous = position + 1;
            } else if n < cuts.len() {
                cuts[n] = position;
                n += 1;
                if n == limit {
                    let mut parts = Vec::with_capacity(n);
                    let mut start = 0;
                    for &cut in &cuts[..n] {
                        parts.push(Value::String(JsStr::new(&text[start..cut])));
                        start = cut + 1;
                    }
                    return parts;
                }
            } else {
                let mut parts = Vec::with_capacity(64.min(limit));
                let mut start = 0;
                for &cut in &cuts {
                    parts.push(Value::String(JsStr::new(&text[start..cut])));
                    start = cut + 1;
                }
                parts.push(Value::String(JsStr::new(&text[start..position])));
                if parts.len() == limit {
                    return parts;
                }
                previous = position + 1;
                overflow_parts = Some(parts);
            }
        }
        if let Some(mut parts) = overflow_parts {
            parts.push(Value::String(JsStr::new(&text[previous..])));
            return parts;
        }
        if n == 0 {
            return vec![Value::String(text.clone())];
        }
        let mut parts = Vec::with_capacity(n + 1);
        let mut start = 0;
        for &cut in &cuts[..n] {
            parts.push(Value::String(JsStr::new(&text[start..cut])));
            start = cut + 1;
        }
        parts.push(Value::String(JsStr::new(&text[start..])));
        parts
    }

    #[inline]
    fn split_ascii_repeat(
        text: &JsStr,
        bytes: &[u8],
        kind: super::pattern::AsciiSplit,
        limit: usize,
    ) -> Vec<Value> {
        use super::pattern::AsciiSplit;
        let matches = |byte: u8| match kind {
            AsciiSplit::One(b0) => byte == b0,
            AsciiSplit::Two(b0, b1) => byte == b0 || byte == b1,
            AsciiSplit::Mask(ascii) => (ascii >> byte) & 1 == 1,
        };
        let mut parts = Vec::with_capacity(8.min(limit));
        let mut previous = 0usize;
        let mut position = 0usize;
        while position < bytes.len() {
            if matches(bytes[position]) {
                parts.push(Value::String(JsStr::new(&text[previous..position])));
                if parts.len() == limit {
                    return parts;
                }
                position += 1;
                while position < bytes.len() && matches(bytes[position]) {
                    position += 1;
                }
                previous = position;
            } else {
                position += 1;
            }
        }
        if previous == 0 && parts.is_empty() {
            parts.push(Value::String(text.clone()));
        } else {
            parts.push(Value::String(JsStr::new(&text[previous..])));
        }
        parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regexp_modifiers_duplicate_names_unicode_sets_and_protocols() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                seen.push(/(?i:a)b/.test("Ab"), /(?i:a)b/.test("AB"));
                const dup = /(?<x>a)\k<x>|(?<x>b)\k<x>/;
                seen.push(dup.exec("bb").groups.x, dup.exec("aa").groups.x);
                const vset = /^[[[a-z]--[aeiou]]&&[a-f]]$/v;
                seen.push(vset.test("b"), vset.test("a"), vset.test("g"));
                const qset = /^[\q{ab|c}]$/v;
                seen.push(qset.test("ab"), qset.test("c"), qset.test("a"));
                seen.push("ab".replace(/(?<n>a)/, "$$<n>-$<n>"));
                seen.push(Object.prototype.toString.call(/x/), RegExp[Symbol.species] === RegExp);
                const it = "a1a2".matchAll(/a(\d)/g);
                seen.push(it.next().value[1], it.next().value[1], it.next().done);
                document.getElementById("out").textContent = seen.join("|");
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(
                "true|false|b|a|true|false|false|true|true|false|$<n>-ab|[object RegExp]|true|1|2|true"
            )
        );
    }
}
