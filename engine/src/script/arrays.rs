// Array.prototype: methods run on engine arrays directly and read array-like receivers,
// such as arguments objects and node lists, through their indexed properties

use super::operations::{is_callable, same_value_zero, strict_equals};
use super::*;
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug)]
pub(super) enum ArrayMethod {
    At,
    Concat,
    CopyWithin,
    Every,
    Fill,
    Filter,
    Find,
    FindIndex,
    FindLast,
    FindLastIndex,
    Flat,
    FlatMap,
    ForEach,
    Includes,
    IndexOf,
    Join,
    LastIndexOf,
    Map,
    Pop,
    Push,
    Reduce,
    ReduceRight,
    Reverse,
    Shift,
    Slice,
    Some,
    Sort,
    Splice,
    ToLocaleString,
    ToReversed,
    ToSorted,
    ToSpliced,
    ToString,
    Unshift,
    With,
}

pub(super) const ARRAY_METHODS: &[(&str, ArrayMethod, u32)] = &[
    ("at", ArrayMethod::At, 1),
    ("concat", ArrayMethod::Concat, 1),
    ("copyWithin", ArrayMethod::CopyWithin, 2),
    ("every", ArrayMethod::Every, 1),
    ("fill", ArrayMethod::Fill, 1),
    ("filter", ArrayMethod::Filter, 1),
    ("find", ArrayMethod::Find, 1),
    ("findIndex", ArrayMethod::FindIndex, 1),
    ("findLast", ArrayMethod::FindLast, 1),
    ("findLastIndex", ArrayMethod::FindLastIndex, 1),
    ("flat", ArrayMethod::Flat, 0),
    ("flatMap", ArrayMethod::FlatMap, 1),
    ("forEach", ArrayMethod::ForEach, 1),
    ("includes", ArrayMethod::Includes, 1),
    ("indexOf", ArrayMethod::IndexOf, 1),
    ("join", ArrayMethod::Join, 1),
    ("lastIndexOf", ArrayMethod::LastIndexOf, 1),
    ("map", ArrayMethod::Map, 1),
    ("pop", ArrayMethod::Pop, 0),
    ("push", ArrayMethod::Push, 1),
    ("reduce", ArrayMethod::Reduce, 1),
    ("reduceRight", ArrayMethod::ReduceRight, 1),
    ("reverse", ArrayMethod::Reverse, 0),
    ("shift", ArrayMethod::Shift, 0),
    ("slice", ArrayMethod::Slice, 2),
    ("some", ArrayMethod::Some, 1),
    ("sort", ArrayMethod::Sort, 1),
    ("splice", ArrayMethod::Splice, 2),
    ("toLocaleString", ArrayMethod::ToLocaleString, 0),
    ("toReversed", ArrayMethod::ToReversed, 0),
    ("toSorted", ArrayMethod::ToSorted, 1),
    ("toSpliced", ArrayMethod::ToSpliced, 2),
    ("toString", ArrayMethod::ToString, 0),
    ("unshift", ArrayMethod::Unshift, 1),
    ("with", ArrayMethod::With, 2),
];

const MAX_SAFE_INT: u64 = 9_007_199_254_740_991;

fn argument(arguments: &[Value], index: usize) -> Value {
    arguments.get(index).cloned().unwrap_or(Value::Undefined)
}

// a relative index argument clamped into 0..=length, as slice and splice resolve them
fn relative(value: f64, length: usize) -> usize {
    let length = length as f64;
    let position = if value < 0.0 {
        (length + value).max(0.0)
    } else {
        value.min(length)
    };
    position as usize
}

fn relative_u64(value: f64, length: u64) -> u64 {
    if value.is_infinite() {
        if value.is_sign_negative() { 0 } else { length }
    } else if value < 0.0 {
        (length as f64 + value).max(0.0) as u64
    } else {
        value.min(length as f64) as u64
    }
}

fn check_length(length: usize) -> Result<(), ScriptError> {
    if length > MAX_RUNTIME_ARRAY_LENGTH {
        return Err(runtime_error(
            "array exceeds the deterministic length limit",
        ));
    }
    Ok(())
}

// why a method may not change an array that preventExtensions, seal or freeze closed: it
// would add, remove or change elements the array no longer lets go
fn array_method_refusal(
    method: ArrayMethod,
    array: &super::ArrayData,
    arguments: &[Value],
) -> Option<String> {
    use super::Integrity;
    use ArrayMethod as M;
    let length = array.len();
    let integrity = array.integrity();
    let fixed_length = array.fixed_length();
    let readonly_length = || {
        (fixed_length || integrity >= Integrity::Frozen).then(|| {
            "Cannot assign to read only property 'length' of object '[object Array]'".to_string()
        })
    };
    let adding = |count: usize| {
        (count > 0 && (integrity >= Integrity::Closed || fixed_length))
            .then(|| format!("Cannot add property {length}, object is not extensible"))
    };
    let removing = |count: usize| {
        (count > 0 && (integrity >= Integrity::Sealed || fixed_length))
            .then(|| format!("Cannot delete property '{}' of [object Array]", length - 1))
    };
    let changing = || {
        (length > 0 && integrity == Integrity::Frozen).then(|| {
            "Cannot assign to read only property '0' of object '[object Array]'".to_string()
        })
    };
    match method {
        M::Push | M::Unshift => adding(arguments.len())
            .or_else(|| {
                (!arguments.is_empty() && matches!(method, M::Unshift))
                    .then(changing)
                    .flatten()
            })
            .or_else(readonly_length),
        M::Pop | M::Shift => removing(length).or_else(readonly_length),
        M::Splice => {
            let inserted = arguments.len().saturating_sub(2);
            let removed = match arguments.get(1) {
                None if arguments.is_empty() => 0,
                None => length,
                Some(Value::Number(count)) => count.max(0.0).min(length as f64) as usize,
                Some(_) => 0,
            };
            adding(inserted.saturating_sub(removed))
                .or_else(|| removing(removed.saturating_sub(inserted)))
                .or_else(|| (inserted + removed > 0).then(changing).flatten())
                .or_else(readonly_length)
        }
        // reversing or sorting a single element writes nothing
        M::Reverse | M::Sort if length < 2 => None,
        _ => changing(),
    }
}

impl ScriptRuntime {
    // whether a hole in an array is still a property: one its property list holds, as an
    // accessor element does, or one a prototype has
    fn holds(&self, this: &Value, key: &str) -> bool {
        matches!(this, Value::Array(array) if array.borrow().own(key).is_some())
            || self.inherits(this, key)
    }

    fn is_plain_array_fast(&self, value: &Value) -> bool {
        let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
        let Value::Array(array) = value else {
            return false;
        };
        unsafe { heap.array_ref(array) }.is_some_and(|a| a.extra.is_none())
            && unsafe { heap.object_ref(&self.intrinsics.array_prototype) }
                .is_some_and(|p| p.array_constructor_intact(heap))
    }

    fn array_create_checked(&mut self, length: u64) -> Result<Value, ScriptError> {
        if length > u32::MAX as u64 {
            return Err(self.range_error("Invalid array length"));
        }
        check_length(length as usize)?;
        Ok(Self::new_array(vec![Value::Uninitialized; length as usize]))
    }

    fn array_species_create(
        &mut self,
        original: &Value,
        length: u64,
    ) -> Result<Value, ScriptError> {
        if self.is_plain_array_fast(original) {
            return self.array_create_checked(length);
        }
        if !self.is_array(original)? {
            return self.array_create_checked(length);
        }
        let mut constructor = self.get_property(original, "constructor")?;
        if !super::operations::is_primitive(&constructor) {
            if let Value::Object(object) = &constructor
                && matches!(
                    object.borrow().class,
                    ObjectClass::Native(ref native) if matches!(**native, Native::ArrayConstructor)
                )
            {
                return self.array_create_checked(length);
            }
            constructor = match self.get_property(&constructor, "@@species")? {
                Value::Null => Value::Undefined,
                species => species,
            };
        }
        match &constructor {
            Value::Undefined => return self.array_create_checked(length),
            Value::Object(object)
                if matches!(
                    object.borrow().class,
                    ObjectClass::Native(ref native) if matches!(**native, Native::ArrayConstructor)
                ) =>
            {
                return self.array_create_checked(length);
            }
            _ => {}
        }
        if !Self::is_constructor(&constructor) {
            return Err(self.type_error("object.constructor[Symbol.species] is not a constructor"));
        }
        self.construct(constructor, vec![Value::Number(length as f64)], None)
    }

    // ArraySpeciesCreate: an array made like the receiver, so a subclass's map or filter
    // hands back the subclass; a plain array, or a constructor without a species, gives one
    // of Array's own
    fn array_species(
        &mut self,
        original: &Value,
        values: Vec<Value>,
        set_length: bool,
    ) -> Result<Value, ScriptError> {
        self.array_species_with_len(original, 0, values, set_length)
    }

    fn array_species_with_len(
        &mut self,
        original: &Value,
        species_len: u64,
        values: Vec<Value>,
        set_length: bool,
    ) -> Result<Value, ScriptError> {
        let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
        if self.is_plain_array_fast(original) {
            return Ok(Value::Array(heap.alloc_array_fast(ArrayData::from(values))));
        }
        if !self.is_array(original)? {
            if species_len > u32::MAX as u64 {
                return Err(self.range_error("Invalid array length"));
            }
            return Ok(Value::Array(heap.alloc_array_fast(ArrayData::from(values))));
        }
        let mut constructor = self.get_property(original, "constructor")?;
        if !super::operations::is_primitive(&constructor) {
            if let Value::Object(object) = &constructor
                && matches!(
                    object.borrow().class,
                    ObjectClass::Native(ref native) if matches!(**native, Native::ArrayConstructor)
                )
            {
                if species_len > u32::MAX as u64 {
                    return Err(self.range_error("Invalid array length"));
                }
                return Ok(Self::new_array(values));
            }
            constructor = match self.get_property(&constructor, "@@species")? {
                Value::Null => Value::Undefined,
                species => species,
            };
        }
        match &constructor {
            Value::Undefined => {
                if species_len > u32::MAX as u64 {
                    return Err(self.range_error("Invalid array length"));
                }
                return Ok(Self::new_array(values));
            }
            Value::Object(object)
                if matches!(
                    object.borrow().class,
                    ObjectClass::Native(ref native) if matches!(**native, Native::ArrayConstructor)
                ) =>
            {
                if species_len > u32::MAX as u64 {
                    return Err(self.range_error("Invalid array length"));
                }
                return Ok(Self::new_array(values));
            }
            _ => {}
        }
        if !Self::is_constructor(&constructor) {
            return Err(self.type_error("object.constructor[Symbol.species] is not a constructor"));
        }
        let made = self.construct(constructor, vec![Value::Number(species_len as f64)], None)?;
        let length = values.len();
        for (index, value) in values.into_iter().enumerate() {
            if matches!(value, Value::Uninitialized) {
                continue;
            }
            self.create_data_property_or_throw(&made, index.to_string(), value)?;
        }
        if set_length {
            self.set_property(&made, "length", Value::Number(length as f64), true)?;
        }
        Ok(made)
    }

    // the elements a method reads: the array itself, or an array-like receiver's items
    fn receiver_values(&mut self, this: &Value, method: &str) -> Result<Vec<Value>, ScriptError> {
        match this {
            Value::Array(array) => Ok(array.borrow().to_vec()),
            Value::Undefined | Value::Null => Err(self.type_error(format!(
                "Array.prototype.{method} called on null or undefined"
            ))),
            Value::String(text) => Ok(text
                .encode_utf16()
                .map(|unit| Value::String(String::from_utf16_lossy(&[unit]).into()))
                .collect()),
            Value::Object(_) => self.array_like_list(this),
            other => {
                // a primitive receiver coerces to its wrapper object, whose
                // missing length reads as nothing to iterate
                if matches!(
                    other,
                    Value::Bool(_) | Value::Number(_) | Value::Symbol(_) | Value::BigInt(_)
                ) {
                    let wrapper = self.coerce_object(other.clone())?;
                    return self.array_like_list(&wrapper);
                }
                match self.iterate_to_vec(other.clone()) {
                    Ok(values) => Ok(values),
                    Err(_) => {
                        self.pending_exception = None;
                        Ok(Vec::new())
                    }
                }
            }
        }
    }

    // the array a mutating method changes; generic receivers are not writable through it
    fn receiver_array(&mut self, this: &Value, method: &str) -> Result<ArrayHandle, ScriptError> {
        match this {
            Value::Array(array) => Ok(array.clone()),
            _ => Err(self.type_error(format!("Array.prototype.{method} needs an array receiver"))),
        }
    }

    // a mutating method on a generic receiver such as a proxy or plain object
    fn mutate_array_like(
        &mut self,
        method: ArrayMethod,
        this: Value,
        arguments: Vec<Value>,
        name: &str,
    ) -> Result<Value, ScriptError> {
        if matches!(method, ArrayMethod::Sort) {
            return self.sort_generic(this, &arguments);
        }
        let this = self.coerce_object(this).map_err(|_| {
            self.type_error(format!(
                "Array.prototype.{name} called on null or undefined"
            ))
        })?;
        let length = self.array_like_length(&this)?;
        match method {
            ArrayMethod::Push => {
                let added = arguments.len() as u64;
                if length.saturating_add(added) > MAX_SAFE_INT {
                    return Err(self.type_error("Array length exceeded"));
                }
                let mut len = length;
                for value in arguments {
                    self.set_property(&this, &len.to_string(), value, true)?;
                    len += 1;
                }
                let new_length = Value::Number(len as f64);
                self.set_property(&this, "length", new_length.clone(), true)?;
                Ok(new_length)
            }
            ArrayMethod::Pop => {
                if length == 0 {
                    self.set_property(&this, "length", Value::Number(0.0), true)?;
                    return Ok(Value::Undefined);
                }
                let new_len = length - 1;
                let key = new_len.to_string();
                let value = self.get_property(&this, &key)?;
                self.delete_property(&this, &key, true)?;
                self.set_property(&this, "length", Value::Number(new_len as f64), true)?;
                Ok(value)
            }
            ArrayMethod::Shift => {
                if length == 0 {
                    self.set_property(&this, "length", Value::Number(0.0), true)?;
                    return Ok(Value::Undefined);
                }
                let first = self.get_property(&this, "0")?;
                let mut k = 1u64;
                let mut steps = 0usize;
                while k < length {
                    steps += 1;
                    check_length(steps)?;
                    let from = k.to_string();
                    let to = (k - 1).to_string();
                    if self.has_property(&this, &from)? {
                        let value = self.get_property(&this, &from)?;
                        self.set_property(&this, &to, value, true)?;
                    } else {
                        self.delete_property(&this, &to, true)?;
                    }
                    k += 1;
                }
                self.delete_property(&this, &(length - 1).to_string(), true)?;
                self.set_property(&this, "length", Value::Number((length - 1) as f64), true)?;
                Ok(first)
            }
            ArrayMethod::Unshift => {
                let arg_count = arguments.len() as u64;
                if arg_count > 0 {
                    if length.saturating_add(arg_count) > MAX_SAFE_INT {
                        return Err(self.type_error("Array length exceeded"));
                    }
                    let mut k = length;
                    let mut steps = 0usize;
                    while k > 0 {
                        steps += 1;
                        check_length(steps)?;
                        let from = (k - 1).to_string();
                        let to = (k + arg_count - 1).to_string();
                        if self.has_property(&this, &from)? {
                            let value = self.get_property(&this, &from)?;
                            self.set_property(&this, &to, value, true)?;
                        } else {
                            self.delete_property(&this, &to, true)?;
                        }
                        k -= 1;
                    }
                    for (j, value) in arguments.into_iter().enumerate() {
                        self.set_property(&this, &j.to_string(), value, true)?;
                    }
                }
                let new_length = Value::Number((length + arg_count) as f64);
                self.set_property(&this, "length", new_length.clone(), true)?;
                Ok(new_length)
            }
            ArrayMethod::Reverse => {
                let middle = length / 2;
                if length > 4096
                    && let Some(indices) = self.sparse_indices(&this, 0, length)
                {
                    let mut pairs: Vec<u64> = indices
                        .into_iter()
                        .map(|idx| {
                            let mirror = length - 1 - idx;
                            idx.min(mirror)
                        })
                        .filter(|&low| low < middle)
                        .collect();
                    pairs.sort_unstable();
                    pairs.dedup();
                    for lower in pairs {
                        let upper = length - 1 - lower;
                        let lower_key = lower.to_string();
                        let upper_key = upper.to_string();
                        let lower_exists = self.has_property(&this, &lower_key)?;
                        let lower_val = if lower_exists {
                            Some(self.get_property(&this, &lower_key)?)
                        } else {
                            None
                        };
                        let upper_exists = self.has_property(&this, &upper_key)?;
                        let upper_val = if upper_exists {
                            Some(self.get_property(&this, &upper_key)?)
                        } else {
                            None
                        };
                        match (lower_val, upper_val) {
                            (Some(lv), Some(uv)) => {
                                self.set_property(&this, &lower_key, uv, true)?;
                                self.set_property(&this, &upper_key, lv, true)?;
                            }
                            (None, Some(uv)) => {
                                self.set_property(&this, &lower_key, uv, true)?;
                                self.delete_property(&this, &upper_key, true)?;
                            }
                            (Some(lv), None) => {
                                self.delete_property(&this, &lower_key, true)?;
                                self.set_property(&this, &upper_key, lv, true)?;
                            }
                            (None, None) => {}
                        }
                    }
                    return Ok(this);
                }
                let mut lower = 0u64;
                let mut steps = 0usize;
                while lower < middle {
                    steps += 1;
                    check_length(steps)?;
                    let upper = length - 1 - lower;
                    let lower_key = lower.to_string();
                    let upper_key = upper.to_string();
                    let lower_exists = self.has_property(&this, &lower_key)?;
                    let lower_val = if lower_exists {
                        Some(self.get_property(&this, &lower_key)?)
                    } else {
                        None
                    };
                    let upper_exists = self.has_property(&this, &upper_key)?;
                    let upper_val = if upper_exists {
                        Some(self.get_property(&this, &upper_key)?)
                    } else {
                        None
                    };
                    match (lower_val, upper_val) {
                        (Some(lv), Some(uv)) => {
                            self.set_property(&this, &lower_key, uv, true)?;
                            self.set_property(&this, &upper_key, lv, true)?;
                        }
                        (None, Some(uv)) => {
                            self.set_property(&this, &lower_key, uv, true)?;
                            self.delete_property(&this, &upper_key, true)?;
                        }
                        (Some(lv), None) => {
                            self.delete_property(&this, &lower_key, true)?;
                            self.set_property(&this, &upper_key, lv, true)?;
                        }
                        (None, None) => {}
                    }
                    lower += 1;
                }
                Ok(this)
            }
            ArrayMethod::Fill => {
                let value = argument(&arguments, 0);
                let start = self.integer(argument(&arguments, 1), 0.0)?;
                let end = match argument(&arguments, 2) {
                    Value::Undefined => length as f64,
                    v => self.integer(v, 0.0)?,
                };
                let mut k = relative_u64(start, length);
                let final_pos = relative_u64(end, length);
                let mut steps = 0usize;
                while k < final_pos {
                    steps += 1;
                    check_length(steps)?;
                    self.set_property(&this, &k.to_string(), value.clone(), true)?;
                    k += 1;
                }
                Ok(this)
            }
            ArrayMethod::CopyWithin => self.copy_within_generic(this, length, &arguments),
            ArrayMethod::Splice => self.splice_generic(this, length, arguments),
            _ => unreachable!("non-mutating method"),
        }
    }

    fn copy_within_generic(
        &mut self,
        this: Value,
        length: u64,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let target = self.integer(argument(arguments, 0), 0.0)?;
        let start = self.integer(argument(arguments, 1), 0.0)?;
        let end = match argument(arguments, 2) {
            Value::Undefined => length as f64,
            value => self.integer(value, 0.0)?,
        };
        let mut to = relative_u64(target, length);
        let mut from = relative_u64(start, length);
        let last = relative_u64(end, length);
        let mut count = last.saturating_sub(from).min(length.saturating_sub(to));
        let backwards = from < to && to < from.saturating_add(count);
        if backwards {
            from += count - 1;
            to += count - 1;
        }
        let mut steps = 0usize;
        while count > 0 {
            steps += 1;
            check_length(steps)?;
            let source = from.to_string();
            let destination = to.to_string();
            if self.has_property(&this, &source)? {
                let value = self.get_property(&this, &source)?;
                self.set_property(&this, &destination, value, true)?;
            } else {
                self.delete_property(&this, &destination, true)?;
            }
            if backwards {
                if count > 1 {
                    from -= 1;
                    to -= 1;
                }
            } else {
                from += 1;
                to += 1;
            }
            count -= 1;
        }
        Ok(this)
    }

    fn splice_generic(
        &mut self,
        this: Value,
        length: u64,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let start = self.integer(argument(&arguments, 0), 0.0)?;
        let actual_start = relative_u64(start, length);
        let (insert_count, actual_delete_count) = match arguments.len() {
            0 => (0u64, 0u64),
            1 => (0u64, length - actual_start),
            n => {
                let dc = self.integer(arguments[1].clone(), 0.0)?;
                let dc = if dc <= 0.0 {
                    0
                } else if dc.is_infinite() {
                    length - actual_start
                } else {
                    (dc as u64).min(length - actual_start)
                };
                ((n - 2) as u64, dc)
            }
        };
        let new_length = length - actual_delete_count + insert_count;
        if new_length > MAX_SAFE_INT {
            return Err(self.type_error("Array length exceeded"));
        }
        let removed = self.array_species_create(&this, actual_delete_count)?;
        let mut k = 0u64;
        let mut steps = 0usize;
        while k < actual_delete_count {
            steps += 1;
            check_length(steps)?;
            let from = (actual_start + k).to_string();
            if self.has_property(&this, &from)? {
                let val = self.get_property(&this, &from)?;
                self.create_data_property_or_throw(&removed, k.to_string(), val)?;
            }
            k += 1;
        }
        self.set_property(
            &removed,
            "length",
            Value::Number(actual_delete_count as f64),
            true,
        )?;
        let items: Vec<Value> = arguments.into_iter().skip(2).collect();
        if insert_count < actual_delete_count {
            let mut k = actual_start;
            let mut steps = 0usize;
            while k < length - actual_delete_count {
                steps += 1;
                check_length(steps)?;
                let from = (k + actual_delete_count).to_string();
                let to = (k + insert_count).to_string();
                if self.has_property(&this, &from)? {
                    let val = self.get_property(&this, &from)?;
                    self.set_property(&this, &to, val, true)?;
                } else {
                    self.delete_property(&this, &to, true)?;
                }
                k += 1;
            }
            let mut k = length;
            steps = 0;
            while k > new_length {
                steps += 1;
                check_length(steps)?;
                k -= 1;
                self.delete_property(&this, &k.to_string(), true)?;
            }
        } else if insert_count > actual_delete_count {
            let mut k = length - actual_delete_count;
            let mut steps = 0usize;
            while k > actual_start {
                steps += 1;
                check_length(steps)?;
                let from = (k + actual_delete_count - 1).to_string();
                let to = (k + insert_count - 1).to_string();
                if self.has_property(&this, &from)? {
                    let val = self.get_property(&this, &from)?;
                    self.set_property(&this, &to, val, true)?;
                } else {
                    self.delete_property(&this, &to, true)?;
                }
                k -= 1;
            }
        }
        for (offset, item) in items.into_iter().enumerate() {
            self.set_property(
                &this,
                &(actual_start + offset as u64).to_string(),
                item,
                true,
            )?;
        }
        self.set_property(&this, "length", Value::Number(new_length as f64), true)?;
        Ok(removed)
    }

    fn sort_generic(&mut self, this: Value, arguments: &[Value]) -> Result<Value, ScriptError> {
        let comparator = argument(arguments, 0);
        if !matches!(comparator, Value::Undefined) && !is_callable(&comparator) {
            return Err(
                self.type_error("The comparison function must be either a function or undefined")
            );
        }
        let object = self
            .coerce_object(this)
            .map_err(|_| self.type_error("Array.prototype.sort called on null or undefined"))?;
        let length = self.array_like_length(&object)?;
        let limit = length.min(MAX_RUNTIME_ARRAY_LENGTH as u64);
        let mut items = Vec::new();
        for k in 0..limit {
            let key = k.to_string();
            if self.has_property(&object, &key)? {
                items.push(self.get_property(&object, &key)?);
            }
        }
        let sorted = self.sort_values(items, &comparator)?;
        let item_count = sorted.len() as u64;
        for (j, value) in sorted.into_iter().enumerate() {
            self.set_property(&object, &j.to_string(), value, true)?;
        }
        for j in item_count..limit {
            self.delete_property(&object, &j.to_string(), true)?;
        }
        Ok(object)
    }

    fn flatten_into_array(
        &mut self,
        target: &Value,
        source: &Value,
        source_len: u64,
        start: u64,
        depth: f64,
        mapper: Option<(&Value, &Value)>,
    ) -> Result<u64, ScriptError> {
        let mut target_index = start;
        let limit = source_len.min(MAX_RUNTIME_ARRAY_LENGTH as u64);
        for source_index in 0..limit {
            let key = source_index.to_string();
            if !self.has_property(source, &key)? {
                continue;
            }
            let mut element = self.get_property(source, &key)?;
            if let Some((map_fn, this_arg)) = mapper {
                element = self.call_function(
                    map_fn.clone(),
                    this_arg.clone(),
                    vec![element, Value::Number(source_index as f64), source.clone()],
                )?;
            }
            let should_flatten = depth > 0.0 && self.is_array(&element)?;
            if should_flatten {
                let next_depth = if depth.is_infinite() {
                    depth
                } else {
                    depth - 1.0
                };
                let element_len = self.array_like_length(&element)?;
                target_index = self.flatten_into_array(
                    target,
                    &element,
                    element_len,
                    target_index,
                    next_depth,
                    None,
                )?;
            } else {
                if target_index >= MAX_SAFE_INT {
                    return Err(self.type_error("Array length exceeded"));
                }
                self.create_data_property_or_throw(target, target_index.to_string(), element)?;
                target_index += 1;
            }
        }
        Ok(target_index)
    }

    pub(super) fn array_like_list(&mut self, value: &Value) -> Result<Vec<Value>, ScriptError> {
        let length = self.get_property(value, "length")?;
        let length = self.coerce_number(length)?;
        let length = if length.is_nan() || length <= 0.0 {
            0
        } else {
            length.min(MAX_RUNTIME_ARRAY_LENGTH as f64) as usize
        };
        let mut values = Vec::with_capacity(length);
        for index in 0..length {
            values.push(self.get_index(value, Value::Number(index as f64))?);
        }
        Ok(values)
    }

    fn callback(&mut self, arguments: &[Value], method: &str) -> Result<Value, ScriptError> {
        let callback = argument(arguments, 0);
        if !is_callable(&callback) {
            return Err(self.type_error(format!(
                "Array.prototype.{method} callback is not a function"
            )));
        }
        Ok(callback)
    }

    fn callback_fast(
        &mut self,
        heap: &super::heap::Heap,
        arguments: &[Value],
        method: &str,
    ) -> Result<Value, ScriptError> {
        let callback = argument(arguments, 0);
        let callable = match &callback {
            Value::Object(object) => unsafe { heap.object_ref(object) }.map_or_else(
                || is_callable(&callback),
                |borrowed| match &borrowed.class {
                    ObjectClass::Function(_) | ObjectClass::Native(_) | ObjectClass::Bound(_) => {
                        true
                    }
                    ObjectClass::Proxy(_) => is_callable(&callback),
                    _ => false,
                },
            ),
            Value::Builtin(_) => true,
            _ => false,
        };
        if !callable {
            return Err(self.type_error(format!(
                "Array.prototype.{method} callback is not a function"
            )));
        }
        Ok(callback)
    }

    pub(super) fn array_like_length(&mut self, value: &Value) -> Result<u64, ScriptError> {
        let length = self.get_property(value, "length")?;
        let number = self.coerce_number(length)?;
        if number.is_nan() || number <= 0.0 {
            Ok(0)
        } else if number.is_infinite() {
            Ok(9_007_199_254_740_991)
        } else {
            Ok((number.trunc() as u64).min(9_007_199_254_740_991))
        }
    }

    fn is_concat_spreadable(&mut self, value: &Value) -> Result<bool, ScriptError> {
        if super::operations::is_primitive(value) {
            return Ok(false);
        }
        let spreadable = self.get_property(value, "@@isConcatSpreadable")?;
        if !matches!(spreadable, Value::Undefined) {
            return Ok(spreadable.truthy());
        }
        self.is_array(value)
    }

    // collects sorted integer keys in [min..max) on a non-proxy object + prototype chain
    // returns none if a proxy is present so callers fall back to dense iteration
    fn sparse_indices(&self, object: &Value, min: u64, max: u64) -> Option<Vec<u64>> {
        let Value::Object(start) = object else {
            return None;
        };
        let mut indices = Vec::new();
        let mut current = Some(start.clone());
        while let Some(obj) = current {
            let borrowed = obj.borrow();
            match &borrowed.class {
                ObjectClass::Proxy(_) => return None,
                ObjectClass::String(text) => {
                    let len = super::strings::utf16_length(text) as u64;
                    let upper = len.min(max);
                    if min < upper {
                        indices.extend(min..upper);
                    }
                }
                ObjectClass::ArrayProto(array) => {
                    let ab = array.borrow();
                    for (idx, val) in ab.iter().enumerate() {
                        let k = idx as u64;
                        if k >= min && k < max && !matches!(val, Value::Uninitialized) {
                            indices.push(k);
                        }
                    }
                    for (name, _) in ab.properties() {
                        if let Some(k) = super::object::array_index(name).map(u64::from)
                            && k >= min
                            && k < max
                        {
                            indices.push(k);
                        }
                    }
                }
                _ => {}
            }
            for key in borrowed.ordered_keys() {
                if let Some(k) = super::object::array_index(&key).map(u64::from) {
                    if k >= min && k < max {
                        indices.push(k);
                    }
                } else if let Ok(k) = key.parse::<u64>()
                    && k < 9_007_199_254_740_991
                    && k.to_string() == key
                    && k >= min
                    && k < max
                {
                    indices.push(k);
                }
            }
            current = borrowed.prototype_or(&self.intrinsics);
        }
        indices.sort_unstable();
        indices.dedup();
        Some(indices)
    }

    fn call_array_callback_generic(
        &mut self,
        method: ArrayMethod,
        this: Value,
        arguments: &[Value],
        name: &str,
    ) -> Result<Value, ScriptError> {
        use ArrayMethod as M;
        let object = self.coerce_object(this)?;
        let length_u64 = self.array_like_length(&object)?;
        let callback = self.callback(arguments, name)?;
        if matches!(method, M::Map | M::Filter) {
            let species_len = if matches!(method, M::Map) {
                length_u64
            } else {
                0
            };
            let target = self.array_species_create(&object, species_len)?;
            let receiver = argument(arguments, 1);
            let mut to = 0u64;
            if length_u64 > 4096
                && let Some(indices) = self.sparse_indices(&object, 0, length_u64)
            {
                let mut cursor = 0u64;
                let mut list = indices;
                let mut pos = 0usize;
                while pos < list.len() {
                    let index = list[pos];
                    pos += 1;
                    if index < cursor || index >= length_u64 {
                        continue;
                    }
                    cursor = index + 1;
                    let key = index.to_string();
                    if !self.has_property(&object, &key)? {
                        continue;
                    }
                    let value = self.get_property(&object, &key)?;
                    let result = self.call_function(
                        callback.clone(),
                        receiver.clone(),
                        vec![value.clone(), Value::Number(index as f64), object.clone()],
                    )?;
                    if matches!(method, M::Map) {
                        self.create_data_property_or_throw(&target, key, result)?;
                    } else if result.truthy() {
                        self.create_data_property_or_throw(&target, to.to_string(), value)?;
                        to += 1;
                    }
                    if let Some(updated) = self.sparse_indices(&object, cursor, length_u64) {
                        list = updated;
                        pos = 0;
                    }
                }
                return Ok(target);
            }
            let length = length_u64.min(MAX_RUNTIME_ARRAY_LENGTH as u64);
            for index in 0..length {
                let key = index.to_string();
                if !self.has_property(&object, &key)? {
                    continue;
                }
                let value = self.get_property(&object, &key)?;
                let result = self.call_function(
                    callback.clone(),
                    receiver.clone(),
                    vec![value.clone(), Value::Number(index as f64), object.clone()],
                )?;
                if matches!(method, M::Map) {
                    self.create_data_property_or_throw(&target, key, result)?;
                } else if result.truthy() {
                    self.create_data_property_or_throw(&target, to.to_string(), value)?;
                    to += 1;
                }
            }
            return Ok(target);
        }
        let receiver = argument(arguments, 1);
        let skip_holes = !matches!(method, M::Find | M::FindIndex);
        let mut mapped = Vec::new();
        if skip_holes
            && length_u64 > 4096
            && let Some(indices) = self.sparse_indices(&object, 0, length_u64)
        {
            if matches!(method, M::Map) {
                check_length(length_u64 as usize)?;
                mapped.resize(length_u64 as usize, Value::Uninitialized);
            }
            let mut cursor = 0u64;
            let mut list = indices;
            let mut pos = 0usize;
            while pos < list.len() {
                let index = list[pos];
                pos += 1;
                if index < cursor || index >= length_u64 {
                    continue;
                }
                cursor = index + 1;
                let key = index.to_string();
                if !self.has_property(&object, &key)? {
                    continue;
                }
                let value = self.get_property(&object, &key)?;
                let result = self.call_function(
                    callback.clone(),
                    receiver.clone(),
                    vec![value.clone(), Value::Number(index as f64), object.clone()],
                )?;
                match method {
                    M::Map => mapped[index as usize] = result,
                    M::Filter if result.truthy() => mapped.push(value),
                    M::Some if result.truthy() => return Ok(Value::Bool(true)),
                    M::Every if !result.truthy() => return Ok(Value::Bool(false)),
                    _ => {}
                }
                if let Some(updated) = self.sparse_indices(&object, cursor, length_u64) {
                    list = updated;
                    pos = 0;
                }
            }
            return Ok(match method {
                M::Map | M::Filter => self.array_species(&object, mapped, false)?,
                M::Some => Value::Bool(false),
                M::Every => Value::Bool(true),
                M::FindIndex => Value::Number(-1.0),
                _ => Value::Undefined,
            });
        }
        let length = length_u64.min(MAX_RUNTIME_ARRAY_LENGTH as u64) as usize;
        for index in 0..length {
            let key = index.to_string();
            if skip_holes && !self.has_property(&object, &key)? {
                if matches!(method, M::Map) {
                    mapped.push(Value::Uninitialized);
                }
                continue;
            }
            let value = self.get_property(&object, &key)?;
            let result = self.call_function(
                callback.clone(),
                receiver.clone(),
                vec![value.clone(), Value::Number(index as f64), object.clone()],
            )?;
            match method {
                M::Map => mapped.push(result),
                M::Filter if result.truthy() => mapped.push(value),
                M::Some if result.truthy() => return Ok(Value::Bool(true)),
                M::Every if !result.truthy() => return Ok(Value::Bool(false)),
                M::Find if result.truthy() => return Ok(value),
                M::FindIndex if result.truthy() => return Ok(Value::Number(index as f64)),
                _ => {}
            }
        }
        Ok(match method {
            M::Map | M::Filter => self.array_species(&object, mapped, false)?,
            M::Some => Value::Bool(false),
            M::Every => Value::Bool(true),
            M::FindIndex => Value::Number(-1.0),
            _ => Value::Undefined,
        })
    }

    fn call_array_reduce_generic(
        &mut self,
        forward: bool,
        this: Value,
        arguments: &[Value],
        name: &str,
    ) -> Result<Value, ScriptError> {
        let object = self.coerce_object(this)?;
        let length_u64 = self.array_like_length(&object)?;
        let callback = self.callback(arguments, name)?;
        let mut accumulator = if arguments.len() >= 2 {
            Some(arguments[1].clone())
        } else {
            None
        };
        if length_u64 > 4096
            && let Some(mut indices) = self.sparse_indices(&object, 0, length_u64)
        {
            if !forward {
                indices.reverse();
            }
            let mut pos = 0usize;
            while pos < indices.len() {
                let index = indices[pos];
                pos += 1;
                let key = index.to_string();
                if !self.has_property(&object, &key)? {
                    continue;
                }
                let value = self.get_property(&object, &key)?;
                match accumulator.take() {
                    None => accumulator = Some(value),
                    Some(acc) => {
                        let next = self.call_function(
                            callback.clone(),
                            Value::Undefined,
                            vec![acc, value, Value::Number(index as f64), object.clone()],
                        )?;
                        accumulator = Some(next);
                        let range = if forward {
                            (index + 1, length_u64)
                        } else {
                            (0, index)
                        };
                        if let Some(mut updated) = self.sparse_indices(&object, range.0, range.1) {
                            if !forward {
                                updated.reverse();
                            }
                            indices = updated;
                            pos = 0;
                        }
                    }
                }
            }
            return accumulator
                .ok_or_else(|| self.type_error("Reduce of empty array with no initial value"));
        }
        let length = length_u64.min(MAX_RUNTIME_ARRAY_LENGTH as u64) as usize;
        let mut step = 0usize;
        while accumulator.is_none() {
            if step >= length {
                return Err(self.type_error("Reduce of empty array with no initial value"));
            }
            let index = if forward { step } else { length - 1 - step };
            step += 1;
            let key = index.to_string();
            if self.has_property(&object, &key)? {
                accumulator = Some(self.get_property(&object, &key)?);
            }
        }
        let mut accumulator = accumulator.unwrap_or(Value::Undefined);
        while step < length {
            let index = if forward { step } else { length - 1 - step };
            step += 1;
            let key = index.to_string();
            if !self.has_property(&object, &key)? {
                continue;
            }
            let value = self.get_property(&object, &key)?;
            accumulator = self.call_function(
                callback.clone(),
                Value::Undefined,
                vec![
                    accumulator,
                    value,
                    Value::Number(index as f64),
                    object.clone(),
                ],
            )?;
        }
        Ok(accumulator)
    }

    fn integer(&mut self, value: Value, default: f64) -> Result<f64, ScriptError> {
        if matches!(value, Value::Undefined) {
            return Ok(default);
        }
        let number = self.coerce_number(value)?;
        Ok(if number.is_nan() { 0.0 } else { number.trunc() })
    }

    // an element read during iteration: arrays may shrink while a callback runs
    // a typed array is read live, so an index its shrunk buffer no longer holds reads as absent
    fn element(this: &Value, values: &[Value], index: usize) -> Option<Value> {
        match this {
            Value::Array(array) => array.borrow().get(index).cloned(),
            Value::TypedArray(array) if index < values.len() => Some(
                super::typed::read_element(&array.borrow(), index).unwrap_or(Value::Uninitialized),
            ),
            _ => values.get(index).cloned(),
        }
    }

    fn element_count(this: &Value, values: &[Value]) -> usize {
        match this {
            Value::Array(array) => array.borrow().len(),
            Value::TypedArray(array) => values
                .len()
                .min(super::typed::element_count(&array.borrow())),
            _ => values.len(),
        }
    }

    pub(super) fn call_array_callback_slice(
        &mut self,
        method: ArrayMethod,
        this: Value,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        use ArrayMethod as M;
        let name = ARRAY_METHODS[method as usize].0;
        if matches!(method, M::Sort) {
            if !matches!(this, Value::Array(_)) {
                return self.mutate_array_like(method, this, arguments.to_vec(), name);
            }
            if let Value::Array(array) = &this
                && let Some(refusal) = array_method_refusal(method, &array.borrow(), arguments)
            {
                return Err(self.type_error(refusal));
            }
        }
        match method {
            M::ForEach | M::Map | M::Filter | M::Some | M::Every | M::Find | M::FindIndex => {
                if !matches!(this, Value::Array(_)) {
                    return self.call_array_callback_generic(method, this, arguments, name);
                }
                let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
                let (values, object, length) = if let Value::Array(array) = &this {
                    let len = unsafe { heap.array_ref(array) }
                        .map_or_else(|| array.borrow().len(), |a| a.len());
                    (Vec::new(), this.clone(), len)
                } else {
                    unreachable!("checked above")
                };
                let callback = self.callback_fast(heap, arguments, name)?;
                if length == 0 {
                    return Ok(match method {
                        M::Map | M::Filter => self.array_species(&this, Vec::new(), false)?,
                        M::Some => Value::Bool(false),
                        M::Every => Value::Bool(true),
                        M::FindIndex => Value::Number(-1.0),
                        _ => Value::Undefined,
                    });
                }
                let fast = Self::simple_closure(&callback);
                let receiver = argument(arguments, 1);
                let mut mapped = if matches!(method, M::Map) && matches!(this, Value::Array(_)) {
                    Vec::with_capacity(length)
                } else {
                    Vec::new()
                };
                #[cfg(all(
                    target_arch = "x86_64",
                    any(target_os = "windows", target_os = "linux")
                ))]
                let ran_jit = if let Some(closure) = &fast
                    && let Some(mut session) = super::jit::JitCallbackSession::try_new(
                        self,
                        closure,
                        callback.clone(),
                        receiver.clone(),
                        length,
                    ) {
                    let param_count = session.param_count();
                    let heap_ptr = session.heap_ptr();
                    let keep_value = matches!(method, M::Filter | M::Find);
                    for index in 0..length {
                        let mut value = if let Value::Array(array) = &this {
                            match unsafe { (&*heap_ptr).array_ref(array) } {
                                Some(data) => {
                                    if index >= data.len() {
                                        Value::Uninitialized
                                    } else if param_count == 0
                                        && !keep_value
                                        && !matches!(data.values[index], Value::Uninitialized)
                                    {
                                        Value::Undefined
                                    } else {
                                        data.values[index].clone()
                                    }
                                }
                                None => {
                                    let borrowed = array.borrow();
                                    if index >= borrowed.len() {
                                        Value::Uninitialized
                                    } else {
                                        borrowed[index].clone()
                                    }
                                }
                            }
                        } else {
                            if index >= Self::element_count(&this, &values) {
                                break;
                            }
                            Self::element(&this, &values, index).unwrap_or(Value::Undefined)
                        };
                        if matches!(value, Value::Uninitialized) {
                            if session.with_runtime(|rt| rt.holds(&this, &index.to_string())) {
                                value = session.with_runtime(|rt| {
                                    rt.get_index(&this, Value::Number(index as f64))
                                })?;
                            } else if matches!(method, M::Find | M::FindIndex) {
                                value = Value::Undefined;
                            } else {
                                if matches!(method, M::Map) {
                                    mapped.push(Value::Uninitialized);
                                }
                                continue;
                            }
                        }
                        let arg0 = if param_count == 0 {
                            Value::Undefined
                        } else if keep_value {
                            value.clone()
                        } else {
                            std::mem::replace(&mut value, Value::Undefined)
                        };
                        let result = match param_count {
                            0 => session.call0()?,
                            1 => session.call1(arg0)?,
                            2 => session.call2(arg0, Value::Number(index as f64))?,
                            _ => {
                                session.call3(arg0, Value::Number(index as f64), object.clone())?
                            }
                        };
                        match method {
                            M::Map => mapped.push(result),
                            M::Filter if result.truthy() => mapped.push(value),
                            M::Some if result.truthy() => return Ok(Value::Bool(true)),
                            M::Every if !result.truthy() => return Ok(Value::Bool(false)),
                            M::Find if result.truthy() => return Ok(value),
                            M::FindIndex if result.truthy() => {
                                return Ok(Value::Number(index as f64));
                            }
                            _ => {}
                        }
                    }
                    true
                } else {
                    false
                };
                #[cfg(not(all(
                    target_arch = "x86_64",
                    any(target_os = "windows", target_os = "linux")
                )))]
                let ran_jit = false;
                if ran_jit {
                    return Ok(match method {
                        M::Map => {
                            self.array_species_with_len(&this, length as u64, mapped, false)?
                        }
                        M::Filter => self.array_species(&this, mapped, false)?,
                        M::Some => Value::Bool(false),
                        M::Every => Value::Bool(true),
                        M::FindIndex => Value::Number(-1.0),
                        _ => Value::Undefined,
                    });
                }
                let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
                for index in 0..length {
                    let value = if let Value::Array(array) = &this {
                        match unsafe { heap.array_ref(array) } {
                            Some(data) => {
                                if index >= data.len() {
                                    Value::Uninitialized
                                } else {
                                    data.values[index].clone()
                                }
                            }
                            None => {
                                let borrowed = array.borrow();
                                if index >= borrowed.len() {
                                    Value::Uninitialized
                                } else {
                                    borrowed[index].clone()
                                }
                            }
                        }
                    } else {
                        if index >= Self::element_count(&this, &values) {
                            break;
                        }
                        Self::element(&this, &values, index).unwrap_or(Value::Undefined)
                    };
                    // an index nothing was put at is passed over, kept as it is by map, and
                    // seen as undefined by the finds
                    let value = match value {
                        Value::Uninitialized if self.holds(&this, &index.to_string()) => {
                            self.get_index(&this, Value::Number(index as f64))?
                        }
                        Value::Uninitialized if matches!(method, M::Find | M::FindIndex) => {
                            Value::Undefined
                        }
                        Value::Uninitialized => {
                            if matches!(method, M::Map) {
                                mapped.push(Value::Uninitialized);
                            }
                            continue;
                        }
                        value => value,
                    };
                    let result = match &fast {
                        Some(closure) => self.call_simple_closure(
                            closure,
                            &callback,
                            receiver.clone(),
                            &[value.clone(), Value::Number(index as f64), object.clone()],
                        )?,
                        None => self.call_function(
                            callback.clone(),
                            receiver.clone(),
                            vec![value.clone(), Value::Number(index as f64), object.clone()],
                        )?,
                    };
                    match method {
                        M::Map => mapped.push(result),
                        M::Filter if result.truthy() => mapped.push(value),
                        M::Some if result.truthy() => return Ok(Value::Bool(true)),
                        M::Every if !result.truthy() => return Ok(Value::Bool(false)),
                        M::Find if result.truthy() => return Ok(value),
                        M::FindIndex if result.truthy() => return Ok(Value::Number(index as f64)),
                        _ => {}
                    }
                }
                Ok(match method {
                    M::Map => self.array_species_with_len(&this, length as u64, mapped, false)?,
                    M::Filter => self.array_species(&this, mapped, false)?,
                    M::Some => Value::Bool(false),
                    M::Every => Value::Bool(true),
                    M::FindIndex => Value::Number(-1.0),
                    _ => Value::Undefined,
                })
            }
            M::Reduce | M::ReduceRight => {
                if !matches!(this, Value::Array(_)) {
                    return self.call_array_reduce_generic(
                        matches!(method, M::Reduce),
                        this,
                        arguments,
                        name,
                    );
                }
                let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
                let (values, object, length) = if let Value::Array(array) = &this {
                    let len = unsafe { heap.array_ref(array) }
                        .map_or_else(|| array.borrow().len(), |a| a.len());
                    (Vec::new(), this.clone(), len)
                } else {
                    unreachable!("checked above")
                };
                let callback = self.callback_fast(heap, arguments, name)?;
                let forward = matches!(method, M::Reduce);
                let mut step = 0usize;
                let mut accumulator = if arguments.len() >= 2 {
                    Some(arguments[1].clone())
                } else {
                    None
                };
                while accumulator.is_none() {
                    if step >= length {
                        return Err(self.type_error("Reduce of empty array with no initial value"));
                    }
                    let first = if forward { step } else { length - 1 - step };
                    step += 1;
                    accumulator = match Self::element(&this, &values, first)
                        .unwrap_or(Value::Uninitialized)
                    {
                        Value::Uninitialized if !self.holds(&this, &first.to_string()) => None,
                        Value::Uninitialized => {
                            Some(self.get_index(&this, Value::Number(first as f64))?)
                        }
                        value => Some(value),
                    };
                }
                let mut accumulator = accumulator.unwrap_or(Value::Undefined);
                let remaining = length - step;
                if remaining == 0 {
                    return Ok(accumulator);
                }
                let fast = Self::simple_closure(&callback);
                #[cfg(all(
                    target_arch = "x86_64",
                    any(target_os = "windows", target_os = "linux")
                ))]
                if let Some(closure) = &fast
                    && let Some(mut session) = super::jit::JitCallbackSession::try_new(
                        self,
                        closure,
                        callback.clone(),
                        Value::Undefined,
                        remaining,
                    )
                {
                    let param_count = session.param_count();
                    let heap_ptr = session.heap_ptr();
                    while step < length {
                        let index = if forward { step } else { length - 1 - step };
                        step += 1;
                        let mut value = if let Value::Array(array) = &this {
                            match unsafe { (&*heap_ptr).array_ref(array) } {
                                Some(data) => data
                                    .values
                                    .get(index)
                                    .cloned()
                                    .unwrap_or(Value::Uninitialized),
                                None => array
                                    .borrow()
                                    .get(index)
                                    .cloned()
                                    .unwrap_or(Value::Uninitialized),
                            }
                        } else {
                            Self::element(&this, &values, index).unwrap_or(Value::Uninitialized)
                        };
                        if matches!(value, Value::Uninitialized) {
                            if !session.with_runtime(|rt| rt.holds(&this, &index.to_string())) {
                                continue;
                            }
                            value = session.with_runtime(|rt| {
                                rt.get_index(&this, Value::Number(index as f64))
                            })?;
                        }
                        accumulator = match param_count {
                            0 => {
                                accumulator.fast_drop();
                                value.fast_drop();
                                session.call0()?
                            }
                            1 => {
                                value.fast_drop();
                                session.call1(accumulator)?
                            }
                            2 => session.call2(accumulator, value)?,
                            3 => session.call3(accumulator, value, Value::Number(index as f64))?,
                            _ => session.call4(
                                accumulator,
                                value,
                                Value::Number(index as f64),
                                object.clone(),
                            )?,
                        };
                    }
                    return Ok(accumulator);
                }
                while step < length {
                    let index = if forward { step } else { length - 1 - step };
                    step += 1;
                    let value =
                        Self::element(&this, &values, index).unwrap_or(Value::Uninitialized);
                    // a callback may have deleted what was there when the method began
                    let value = match value {
                        Value::Uninitialized if !self.holds(&this, &index.to_string()) => {
                            continue;
                        }
                        Value::Uninitialized => {
                            self.get_index(&this, Value::Number(index as f64))?
                        }
                        value => value,
                    };
                    accumulator = match &fast {
                        Some(closure) => self.call_simple_closure(
                            closure,
                            &callback,
                            Value::Undefined,
                            &[
                                accumulator,
                                value,
                                Value::Number(index as f64),
                                object.clone(),
                            ],
                        )?,
                        None => self.call_function(
                            callback.clone(),
                            Value::Undefined,
                            vec![
                                accumulator,
                                value,
                                Value::Number(index as f64),
                                object.clone(),
                            ],
                        )?,
                    };
                }
                Ok(accumulator)
            }
            M::Sort => {
                let comparator = argument(arguments, 0);
                if !matches!(comparator, Value::Undefined) && !is_callable(&comparator) {
                    return Err(self.type_error(
                        "The comparison function must be either a function or undefined",
                    ));
                }
                if let Value::Array(array) = &this {
                    let is_dense_plain = {
                        let borrowed = array.borrow();
                        borrowed.extra.is_none()
                            && !borrowed.iter().any(|v| matches!(v, Value::Uninitialized))
                    };
                    if is_dense_plain {
                        let values = array.borrow().to_vec();
                        let sorted = self.sort_values(values, &comparator)?;
                        **array.borrow_mut() = sorted;
                        return Ok(this);
                    }
                }
                self.sort_generic(this, arguments)
            }
            M::ToSorted => {
                let comparator = argument(arguments, 0);
                if !matches!(comparator, Value::Undefined) && !is_callable(&comparator) {
                    return Err(self.type_error(
                        "The comparison function must be either a function or undefined",
                    ));
                }
                let object = self.coerce_object(this).map_err(|_| {
                    self.type_error("Array.prototype.toSorted called on null or undefined")
                })?;
                let len = self.array_like_length(&object)?;
                if len > u32::MAX as u64 {
                    return Err(self.range_error("Invalid array length"));
                }
                check_length(len as usize)?;
                let mut values = Vec::with_capacity(len as usize);
                for k in 0..len {
                    values.push(self.get_property(&object, &k.to_string())?);
                }
                let sorted = self.sort_values(values, &comparator)?;
                Ok(Self::new_array(sorted))
            }
            _ => unreachable!("unexpected array callback method"),
        }
    }

    pub(super) fn call_array_method(
        &mut self,
        method: ArrayMethod,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use ArrayMethod as M;
        if matches!(
            method,
            M::ForEach
                | M::Map
                | M::Filter
                | M::Some
                | M::Every
                | M::Find
                | M::FindIndex
                | M::Reduce
                | M::ReduceRight
                | M::Sort
                | M::ToSorted
        ) {
            return self.call_array_callback_slice(method, this, &arguments);
        }
        let name = ARRAY_METHODS[method as usize].0;
        let mutates = matches!(
            method,
            M::Push
                | M::Pop
                | M::Shift
                | M::Unshift
                | M::Splice
                | M::Reverse
                | M::Sort
                | M::Fill
                | M::CopyWithin
        );
        if mutates {
            let generic = match &this {
                Value::Array(array) => {
                    let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
                    !self.array_protos_clean(heap) || array.borrow().extra.is_some()
                }
                _ => true,
            };
            if generic {
                if let Value::Array(array) = &this
                    && let Some(refusal) = array_method_refusal(method, &array.borrow(), &arguments)
                {
                    return Err(self.type_error(refusal));
                }
                return self.mutate_array_like(method, this, arguments, name);
            }
        }
        match method {
            M::ForEach
            | M::Map
            | M::Filter
            | M::Some
            | M::Every
            | M::Find
            | M::FindIndex
            | M::Reduce
            | M::ReduceRight
            | M::Sort
            | M::ToSorted => unreachable!(),
            M::FindLast | M::FindLastIndex => {
                if !matches!(this, Value::Array(_)) {
                    let object = self.coerce_object(this)?;
                    let length = self.array_like_length(&object)?;
                    let callback = self.callback(&arguments, name)?;
                    let receiver = argument(&arguments, 1);
                    let mut index = length;
                    let mut steps = 0usize;
                    while index > 0 {
                        steps += 1;
                        check_length(steps)?;
                        index -= 1;
                        let value = self.get_property(&object, &index.to_string())?;
                        let result = self.call_function(
                            callback.clone(),
                            receiver.clone(),
                            vec![value.clone(), Value::Number(index as f64), object.clone()],
                        )?;
                        if result.truthy() {
                            return Ok(match method {
                                M::FindLast => value,
                                _ => Value::Number(index as f64),
                            });
                        }
                    }
                    return Ok(match method {
                        M::FindLast => Value::Undefined,
                        _ => Value::Number(-1.0),
                    });
                }
                let (values, object, length) = if let Value::Array(array) = &this {
                    (Vec::new(), this.clone(), array.borrow().len())
                } else {
                    unreachable!("checked above")
                };
                let callback = self.callback(&arguments, name)?;
                let receiver = argument(&arguments, 1);
                for index in (0..length).rev() {
                    let value = Self::element(&this, &values, index)
                        .unwrap_or(Value::Undefined)
                        .filled();
                    let result = self.call_function(
                        callback.clone(),
                        receiver.clone(),
                        vec![value.clone(), Value::Number(index as f64), object.clone()],
                    )?;
                    if result.truthy() {
                        return Ok(match method {
                            M::FindLast => value,
                            _ => Value::Number(index as f64),
                        });
                    }
                }
                Ok(match method {
                    M::FindLast => Value::Undefined,
                    _ => Value::Number(-1.0),
                })
            }
            M::Includes | M::IndexOf | M::LastIndexOf => {
                if !matches!(this, Value::Array(_)) {
                    let object = self.coerce_object(this)?;
                    let length_u64 = self.array_like_length(&object)?;
                    if length_u64 == 0 {
                        return Ok(match method {
                            M::Includes => Value::Bool(false),
                            _ => Value::Number(-1.0),
                        });
                    }
                    let target = argument(&arguments, 0);
                    if matches!(method, M::LastIndexOf) {
                        let start = if arguments.len() >= 2 {
                            let from = self.integer(arguments[1].clone(), 0.0)?;
                            if from < 0.0 {
                                length_u64 as f64 + from
                            } else {
                                from.min(length_u64 as f64 - 1.0)
                            }
                        } else {
                            length_u64 as f64 - 1.0
                        };
                        if start < 0.0 {
                            return Ok(Value::Number(-1.0));
                        }
                        let start_u64 = start as u64;
                        if start_u64 > 4096
                            && let Some(mut indices) =
                                self.sparse_indices(&object, 0, start_u64 + 1)
                        {
                            indices.reverse();
                            for index in indices {
                                let key = index.to_string();
                                if self.has_property(&object, &key)? {
                                    let value = self.get_property(&object, &key)?;
                                    if strict_equals(&value, &target) {
                                        return Ok(Value::Number(index as f64));
                                    }
                                }
                            }
                            return Ok(Value::Number(-1.0));
                        }
                        let mut index = start_u64;
                        loop {
                            let key = index.to_string();
                            if self.has_property(&object, &key)? {
                                let value = self.get_property(&object, &key)?;
                                if strict_equals(&value, &target) {
                                    return Ok(Value::Number(index as f64));
                                }
                            }
                            if index == 0 {
                                break;
                            }
                            index -= 1;
                        }
                        return Ok(Value::Number(-1.0));
                    }
                    let from = self.integer(argument(&arguments, 1), 0.0)?;
                    let start_f = if from < 0.0 {
                        (length_u64 as f64 + from).max(0.0)
                    } else {
                        from.min(length_u64 as f64)
                    };
                    let start_u64 = start_f as u64;
                    if start_u64 >= length_u64 {
                        return Ok(match method {
                            M::Includes => Value::Bool(false),
                            _ => Value::Number(-1.0),
                        });
                    }
                    if length_u64.saturating_sub(start_u64) > 4096
                        && let Some(indices) = self.sparse_indices(&object, start_u64, length_u64)
                    {
                        if matches!(method, M::Includes) {
                            if same_value_zero(&Value::Undefined, &target)
                                && (indices.len() as u64) < length_u64 - start_u64
                            {
                                return Ok(Value::Bool(true));
                            }
                            for index in indices {
                                let value = self.get_property(&object, &index.to_string())?;
                                if same_value_zero(&value, &target) {
                                    return Ok(Value::Bool(true));
                                }
                            }
                            return Ok(Value::Bool(false));
                        }
                        for index in indices {
                            let key = index.to_string();
                            if self.has_property(&object, &key)? {
                                let value = self.get_property(&object, &key)?;
                                if strict_equals(&value, &target) {
                                    return Ok(Value::Number(index as f64));
                                }
                            }
                        }
                        return Ok(Value::Number(-1.0));
                    }
                    let mut index = start_u64;
                    let mut steps = 0usize;
                    while index < length_u64 {
                        steps += 1;
                        check_length(steps)?;
                        let key = index.to_string();
                        if !matches!(method, M::Includes) && !self.has_property(&object, &key)? {
                            index += 1;
                            continue;
                        }
                        let value = self.get_property(&object, &key)?;
                        let found = match method {
                            M::Includes => same_value_zero(&value, &target),
                            _ => strict_equals(&value, &target),
                        };
                        if found {
                            return Ok(match method {
                                M::Includes => Value::Bool(true),
                                _ => Value::Number(index as f64),
                            });
                        }
                        index += 1;
                    }
                    return Ok(match method {
                        M::Includes => Value::Bool(false),
                        _ => Value::Number(-1.0),
                    });
                }
                let Value::Array(array) = &this else {
                    unreachable!("checked above")
                };
                let length = array.borrow().len();
                if length == 0 {
                    return Ok(match method {
                        M::Includes => Value::Bool(false),
                        _ => Value::Number(-1.0),
                    });
                }
                let target = argument(&arguments, 0);
                if matches!(method, M::LastIndexOf) {
                    let start = if arguments.len() >= 2 {
                        let from = self.integer(arguments[1].clone(), 0.0)?;
                        if from < 0.0 {
                            length as f64 + from
                        } else {
                            from.min(length as f64 - 1.0)
                        }
                    } else {
                        length as f64 - 1.0
                    };
                    if start < 0.0 {
                        return Ok(Value::Number(-1.0));
                    }
                    for index in (0..=start as usize).rev() {
                        let raw = array
                            .borrow()
                            .get(index)
                            .cloned()
                            .unwrap_or(Value::Uninitialized);
                        let value = match raw {
                            Value::Uninitialized if !self.holds(&this, &index.to_string()) => {
                                continue;
                            }
                            Value::Uninitialized => {
                                self.get_index(&this, Value::Number(index as f64))?
                            }
                            value => value,
                        };
                        if strict_equals(&value, &target) {
                            return Ok(Value::Number(index as f64));
                        }
                    }
                    return Ok(Value::Number(-1.0));
                }
                let from = self.integer(argument(&arguments, 1), 0.0)?;
                let start = relative(from, length);
                for index in start..length {
                    let raw = array
                        .borrow()
                        .get(index)
                        .cloned()
                        .unwrap_or(Value::Uninitialized);
                    let value = match raw {
                        Value::Uninitialized if self.holds(&this, &index.to_string()) => {
                            self.get_index(&this, Value::Number(index as f64))?
                        }
                        Value::Uninitialized if matches!(method, M::Includes) => Value::Undefined,
                        Value::Uninitialized => continue,
                        value => value,
                    };
                    let found = match method {
                        M::Includes => same_value_zero(&value, &target),
                        _ => strict_equals(&value, &target),
                    };
                    if found {
                        return Ok(match method {
                            M::Includes => Value::Bool(true),
                            _ => Value::Number(index as f64),
                        });
                    }
                }
                Ok(match method {
                    M::Includes => Value::Bool(false),
                    _ => Value::Number(-1.0),
                })
            }
            M::ToString => {
                let object = self.coerce_object(this)?;
                let func = self.get_property(&object, "join")?;
                if is_callable(&func) {
                    self.call_function(func, object, Vec::new())
                } else {
                    Ok(Value::String(self.object_to_string(&object)?.into()))
                }
            }
            M::ToLocaleString => {
                let object = self.coerce_object(this)?;
                let len = self.array_like_length(&object)?;
                if let Value::Array(array) = &object {
                    if self
                        .joining
                        .iter()
                        .any(|joining| ArrayHandle::ptr_eq(joining, array))
                    {
                        return Ok(Value::String(JsStr::default()));
                    }
                    self.joining.push(array.clone());
                }
                let limit = len.min(MAX_RUNTIME_ARRAY_LENGTH as u64);
                let mut parts = Vec::with_capacity(limit as usize);
                let mut outcome = Ok(());
                for k in 0..limit {
                    let elem = match self.get_property(&object, &k.to_string()) {
                        Ok(elem) => elem,
                        Err(error) => {
                            outcome = Err(error);
                            break;
                        }
                    };
                    if matches!(elem, Value::Undefined | Value::Null | Value::Uninitialized) {
                        parts.push(String::new());
                    } else {
                        let to_locale = match self.get_property(&elem, "toLocaleString") {
                            Ok(method) => method,
                            Err(error) => {
                                outcome = Err(error);
                                break;
                            }
                        };
                        if !is_callable(&to_locale) {
                            outcome = Err(self.type_error("toLocaleString is not a function"));
                            break;
                        }
                        let rendered = match self
                            .call_function(to_locale, elem, arguments.clone())
                            .and_then(|value| self.coerce_string(value))
                        {
                            Ok(text) => text,
                            Err(error) => {
                                outcome = Err(error);
                                break;
                            }
                        };
                        parts.push(rendered);
                    }
                }
                if matches!(&object, Value::Array(_)) {
                    self.joining.pop();
                }
                outcome.map(|()| Value::String(parts.join(",").into()))
            }
            M::Join => {
                if let Value::Array(array) = &this {
                    let separator = match argument(&arguments, 0) {
                        Value::Undefined => std::borrow::Cow::Borrowed(","),
                        separator => std::borrow::Cow::Owned(self.coerce_string(separator)?),
                    };
                    if self
                        .joining
                        .iter()
                        .any(|joining| ArrayHandle::ptr_eq(joining, array))
                    {
                        return Ok(Value::String(JsStr::default()));
                    }
                    let fast_plain = {
                        let borrowed = array.borrow();
                        borrowed.extra.is_none()
                            && !borrowed.iter().any(|v| matches!(v, Value::Uninitialized))
                    };
                    if fast_plain {
                        return Ok(Value::String(self.join_array(array, &separator)?.into()));
                    }
                    let len = array.borrow().len() as u64;
                    self.joining.push(array.clone());
                    let limit = len.min(MAX_RUNTIME_ARRAY_LENGTH as u64);
                    let mut parts = Vec::with_capacity(limit as usize);
                    let mut outcome = Ok(());
                    for k in 0..limit {
                        let elem = match self.get_property(&this, &k.to_string()) {
                            Ok(elem) => elem,
                            Err(error) => {
                                outcome = Err(error);
                                break;
                            }
                        };
                        match elem {
                            Value::Undefined | Value::Null | Value::Uninitialized => {
                                parts.push(String::new())
                            }
                            value => match self.coerce_string(value) {
                                Ok(part) => parts.push(part),
                                Err(error) => {
                                    outcome = Err(error);
                                    break;
                                }
                            },
                        }
                    }
                    self.joining.pop();
                    return outcome.map(|()| Value::String(parts.join(&separator).into()));
                }
                let object = self.coerce_object(this)?;
                let len = self.array_like_length(&object)?;
                let separator = match argument(&arguments, 0) {
                    Value::Undefined => std::borrow::Cow::Borrowed(","),
                    separator => std::borrow::Cow::Owned(self.coerce_string(separator)?),
                };
                let limit = len.min(MAX_RUNTIME_ARRAY_LENGTH as u64);
                let mut parts = Vec::with_capacity(limit as usize);
                for k in 0..limit {
                    let elem = self.get_property(&object, &k.to_string())?;
                    match elem {
                        Value::Undefined | Value::Null | Value::Uninitialized => {
                            parts.push(String::new())
                        }
                        value => parts.push(self.coerce_string(value)?),
                    }
                }
                Ok(Value::String(parts.join(&separator).into()))
            }
            M::At => {
                if !matches!(this, Value::Array(_)) {
                    let object = self.coerce_object(this)?;
                    let length = self.array_like_length(&object)? as f64;
                    let index = self.integer(argument(&arguments, 0), 0.0)?;
                    let k = if index < 0.0 { length + index } else { index };
                    if k < 0.0 || k >= length {
                        return Ok(Value::Undefined);
                    }
                    return self.get_property(&object, &(k as u64).to_string());
                }
                let values = self.receiver_values(&this, name)?;
                let index = self.integer(argument(&arguments, 0), 0.0)?;
                let index = if index < 0.0 {
                    values.len() as f64 + index
                } else {
                    index
                };
                if index < 0.0 || index >= values.len() as f64 {
                    return Ok(Value::Undefined);
                }
                Ok(values[index as usize].clone().filled())
            }
            M::Concat => {
                let object = self.coerce_object(this)?;
                let fast = self.is_plain_array_fast(&object);
                let target = if fast {
                    None
                } else {
                    Some(self.array_species_create(&object, 0)?)
                };
                let mut result = Vec::new();
                let mut n = 0u64;
                let mut items = Vec::with_capacity(arguments.len() + 1);
                items.push(object.clone());
                items.extend(arguments);
                for item in items {
                    if self.is_concat_spreadable(&item)? {
                        if let Some(target) = &target {
                            let len = self.array_like_length(&item)?;
                            if n.saturating_add(len) > MAX_SAFE_INT {
                                return Err(self.type_error("Array length exceeded"));
                            }
                            let limit = len.min(MAX_RUNTIME_ARRAY_LENGTH as u64);
                            for k in 0..limit {
                                let key = k.to_string();
                                if self.has_property(&item, &key)? {
                                    let sub = self.get_property(&item, &key)?;
                                    self.create_data_property_or_throw(target, n.to_string(), sub)?;
                                }
                                n += 1;
                            }
                        } else if let Value::Array(array) = &item
                            && array.borrow().properties().is_empty()
                        {
                            let len = array.borrow().len();
                            check_length(result.len().saturating_add(len))?;
                            for index in 0..len {
                                let value = array.borrow()[index].clone();
                                if matches!(value, Value::Uninitialized)
                                    && self.holds(&item, &index.to_string())
                                {
                                    result
                                        .push(self.get_index(&item, Value::Number(index as f64))?);
                                } else {
                                    result.push(value);
                                }
                            }
                        } else if let Value::TypedArray(array) = &item {
                            let len_u64 = self.array_like_length(&item)?;
                            if (result.len() as u64).saturating_add(len_u64) > MAX_SAFE_INT {
                                return Err(self.type_error("Array length exceeded"));
                            }
                            let len = len_u64.min(MAX_RUNTIME_ARRAY_LENGTH as u64) as usize;
                            check_length(result.len().saturating_add(len))?;
                            let borrowed = array.borrow();
                            let count = super::typed::element_count(&borrowed);
                            let present = len.min(count);
                            for index in 0..present {
                                result.push(
                                    super::typed::read_element(&borrowed, index)
                                        .unwrap_or(Value::Undefined),
                                );
                            }
                            drop(borrowed);
                            for index in present..len {
                                let key = index.to_string();
                                if self.has_property(&item, &key)? {
                                    result.push(self.get_property(&item, &key)?);
                                } else {
                                    result.push(Value::Uninitialized);
                                }
                            }
                        } else {
                            let len = self.array_like_length(&item)?;
                            if (result.len() as u64).saturating_add(len) > MAX_SAFE_INT {
                                return Err(self.type_error("Array length exceeded"));
                            }
                            let len = len.min(MAX_RUNTIME_ARRAY_LENGTH as u64) as usize;
                            check_length(result.len().saturating_add(len))?;
                            for index in 0..len {
                                let key = index.to_string();
                                if self.has_property(&item, &key)? {
                                    result.push(self.get_property(&item, &key)?);
                                } else {
                                    result.push(Value::Uninitialized);
                                }
                            }
                        }
                    } else if let Some(target) = &target {
                        if n >= MAX_SAFE_INT {
                            return Err(self.type_error("Array length exceeded"));
                        }
                        self.create_data_property_or_throw(target, n.to_string(), item)?;
                        n += 1;
                    } else {
                        check_length(result.len().saturating_add(1))?;
                        result.push(item);
                    }
                }
                if let Some(target) = target {
                    self.set_property(&target, "length", Value::Number(n as f64), true)?;
                    Ok(target)
                } else {
                    Ok(Self::new_array(result))
                }
            }
            M::Slice => {
                if self.is_plain_array_fast(&this)
                    && let Value::Array(array) = &this
                {
                    let length = array.borrow().len();
                    let start = self.integer(argument(&arguments, 0), 0.0)?;
                    let end = self.integer(argument(&arguments, 1), length as f64)?;
                    let start = relative(start, length);
                    let end = relative(end, length);
                    let can_fast = {
                        let borrowed = array.borrow();
                        borrowed.extra.is_none()
                            && borrowed.len() >= end
                            && (start..end).all(|i| !matches!(borrowed[i], Value::Uninitialized))
                    };
                    if can_fast {
                        let picked = if start < end {
                            array.borrow()[start..end].to_vec()
                        } else {
                            Vec::new()
                        };
                        return Ok(Self::new_array(picked));
                    }
                }
                let object = self.coerce_object(this)?;
                let length = self.array_like_length(&object)?;
                let start = self.integer(argument(&arguments, 0), 0.0)?;
                let end = match argument(&arguments, 1) {
                    Value::Undefined => length as f64,
                    value => self.integer(value, 0.0)?,
                };
                let mut k = relative_u64(start, length);
                let final_pos = relative_u64(end, length);
                let count = final_pos.saturating_sub(k);
                let target = self.array_species_create(&object, count)?;
                let mut n = 0u64;
                if let Value::TypedArray(array) = &object {
                    while k < final_pos {
                        let val = super::typed::read_element(&array.borrow(), k as usize);
                        if let Some(val) = val {
                            self.create_data_property_or_throw(&target, n.to_string(), val)?;
                        }
                        k += 1;
                        n += 1;
                    }
                } else {
                    let mut steps = 0usize;
                    while k < final_pos {
                        steps += 1;
                        check_length(steps)?;
                        let key = k.to_string();
                        if self.has_property(&object, &key)? {
                            let val = self.get_property(&object, &key)?;
                            self.create_data_property_or_throw(&target, n.to_string(), val)?;
                        }
                        k += 1;
                        n += 1;
                    }
                }
                self.set_property(&target, "length", Value::Number(n as f64), true)?;
                Ok(target)
            }
            M::Flat | M::FlatMap => {
                let object = self.coerce_object(this)?;
                let source_len = self.array_like_length(&object)?;
                let (depth, mapper) = if matches!(method, M::FlatMap) {
                    (
                        1.0,
                        Some((self.callback(&arguments, name)?, argument(&arguments, 1))),
                    )
                } else {
                    let d = match argument(&arguments, 0) {
                        Value::Undefined => 1.0,
                        v => self.integer(v, 1.0)?,
                    };
                    (d, None)
                };
                let target = self.array_species_create(&object, 0)?;
                self.flatten_into_array(
                    &target,
                    &object,
                    source_len,
                    0,
                    depth,
                    mapper.as_ref().map(|(m, t)| (m, t)),
                )?;
                Ok(target)
            }
            M::Push => {
                let array = self.receiver_array(&this, name)?;
                let mut array = array.borrow_mut();
                check_length(array.len().saturating_add(arguments.len()))?;
                // keep 1-2 element arrays compact instead of jumping straight to capacity 4
                let needed = array.len() + arguments.len();
                if needed <= 2 && array.capacity() < needed {
                    array.reserve_exact(arguments.len());
                }
                array.extend(arguments);
                Ok(Value::Number(array.len() as f64))
            }
            M::Pop => {
                let array = self.receiver_array(&this, name)?;
                let value = array.borrow_mut().pop();
                Ok(value.unwrap_or(Value::Undefined).filled())
            }
            M::Shift => {
                let array = self.receiver_array(&this, name)?;
                let mut array = array.borrow_mut();
                Ok(if array.is_empty() {
                    Value::Undefined
                } else {
                    array.remove(0).filled()
                })
            }
            M::Unshift => {
                let array = self.receiver_array(&this, name)?;
                let mut array = array.borrow_mut();
                check_length(array.len().saturating_add(arguments.len()))?;
                array.splice(0..0, arguments);
                Ok(Value::Number(array.len() as f64))
            }
            M::Reverse => {
                let array = self.receiver_array(&this, name)?;
                array.borrow_mut().reverse();
                Ok(this)
            }
            M::ToReversed => {
                let object = self.coerce_object(this)?;
                let len = self.array_like_length(&object)?;
                if len > u32::MAX as u64 {
                    return Err(self.range_error("Invalid array length"));
                }
                check_length(len as usize)?;
                let mut values = Vec::with_capacity(len as usize);
                for k in (0..len).rev() {
                    values.push(self.get_property(&object, &k.to_string())?);
                }
                Ok(Self::new_array(values))
            }
            M::Fill => {
                let array = self.receiver_array(&this, name)?;
                let length = array.borrow().len();
                let value = argument(&arguments, 0);
                let start = self.integer(argument(&arguments, 1), 0.0)?;
                let end = self.integer(argument(&arguments, 2), length as f64)?;
                let (start, end) = (relative(start, length), relative(end, length));
                for slot in array.borrow_mut().iter_mut().take(end).skip(start) {
                    *slot = value.clone();
                }
                Ok(this)
            }
            M::CopyWithin => {
                let array = self.receiver_array(&this, name)?;
                let length = array.borrow().len() as u64;
                self.copy_within_generic(this, length, &arguments)
            }
            M::Splice => {
                if self.is_plain_array_fast(&this)
                    && let Value::Array(array) = &this
                {
                    let length = array.borrow().len();
                    let start = self.integer(argument(&arguments, 0), 0.0)?;
                    let start = relative(start, length);
                    let delete = match arguments.len() {
                        0 => 0,
                        1 => length - start,
                        _ => {
                            let count = self.integer(arguments[1].clone(), 0.0)?;
                            count.clamp(0.0, (length - start) as f64) as usize
                        }
                    };
                    let can_fast = {
                        let borrowed = array.borrow();
                        borrowed.extra.is_none()
                            && borrowed.len() == length
                            && !borrowed.iter().any(|v| matches!(v, Value::Uninitialized))
                    };
                    if can_fast {
                        let insert: Vec<Value> = arguments.iter().skip(2).cloned().collect();
                        check_length(length - delete + insert.len())?;
                        let mut values = array.borrow().to_vec();
                        let removed: Vec<Value> =
                            values.splice(start..start + delete, insert).collect();
                        **array.borrow_mut() = values;
                        return Ok(Self::new_array(removed));
                    }
                }
                let length = self.array_like_length(&this)?;
                self.splice_generic(this, length, arguments)
            }
            M::ToSpliced => {
                let object = self.coerce_object(this)?;
                let len = self.array_like_length(&object)?;
                let start = self.integer(argument(&arguments, 0), 0.0)?;
                let actual_start = relative_u64(start, len);
                let (insert_count, skip_count) = match arguments.len() {
                    0 => (0u64, 0u64),
                    1 => (0u64, len - actual_start),
                    n => {
                        let dc = self.integer(arguments[1].clone(), 0.0)?;
                        let dc = if dc <= 0.0 {
                            0
                        } else if dc.is_infinite() {
                            len - actual_start
                        } else {
                            (dc as u64).min(len - actual_start)
                        };
                        ((n - 2) as u64, dc)
                    }
                };
                let new_len = len + insert_count - skip_count;
                if new_len > MAX_SAFE_INT {
                    return Err(self.type_error("Array length exceeded"));
                }
                if new_len > u32::MAX as u64 {
                    return Err(self.range_error("Invalid array length"));
                }
                check_length(new_len as usize)?;
                let mut result = Vec::with_capacity(new_len as usize);
                for k in 0..actual_start {
                    result.push(self.get_property(&object, &k.to_string())?);
                }
                for item in arguments.into_iter().skip(2) {
                    result.push(item);
                }
                for k in (actual_start + skip_count)..len {
                    result.push(self.get_property(&object, &k.to_string())?);
                }
                Ok(Self::new_array(result))
            }
            M::With => {
                let object = self.coerce_object(this)?;
                let len = self.array_like_length(&object)?;
                let relative_index = self.integer(argument(&arguments, 0), 0.0)?;
                let actual = if relative_index < 0.0 {
                    len as f64 + relative_index
                } else {
                    relative_index
                };
                if actual < 0.0 || actual >= len as f64 {
                    return Err(self.range_error("Invalid index"));
                }
                if len > u32::MAX as u64 {
                    return Err(self.range_error("Invalid array length"));
                }
                check_length(len as usize)?;
                let target = actual as u64;
                let replacement = argument(&arguments, 1);
                let mut result = Vec::with_capacity(len as usize);
                for k in 0..len {
                    if k == target {
                        result.push(replacement.clone());
                    } else {
                        result.push(self.get_property(&object, &k.to_string())?);
                    }
                }
                Ok(Self::new_array(result))
            }
        }
    }

    // a stable merge sort whose comparator may run script and throw; undefined sorts last
    pub(super) fn sort_values(
        &mut self,
        values: Vec<Value>,
        comparator: &Value,
    ) -> Result<Vec<Value>, ScriptError> {
        // indexes nothing was put at go last with them
        let (mut defined, undefined): (Vec<Value>, Vec<Value>) = values
            .into_iter()
            .partition(|value| !matches!(value, Value::Undefined | Value::Uninitialized));
        // default ordering compares string forms, computed once per element
        let keys: Option<Vec<Vec<u16>>> = if matches!(comparator, Value::Undefined) {
            let mut keys = Vec::with_capacity(defined.len());
            for value in &defined {
                keys.push(self.coerce_string(value.clone())?.encode_utf16().collect());
            }
            Some(keys)
        } else {
            None
        };
        let fast_comparator = Self::simple_closure(comparator);
        let mut order: Vec<usize> = (0..defined.len()).collect();
        let mut scratch = order.clone();
        #[cfg(all(
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        let sorted_via_jit = if defined.len() >= 2
            && let Some(closure) = &fast_comparator
            && let Some(mut session) = super::jit::JitCallbackSession::try_new(
                self,
                closure,
                comparator.clone(),
                Value::Undefined,
                defined.len(),
            ) {
            merge_sort_jit(&mut session, &mut order, &mut scratch, &defined)?;
            true
        } else {
            false
        };
        #[cfg(not(all(
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        )))]
        let sorted_via_jit = false;
        if !sorted_via_jit {
            self.merge_sort(
                &mut order,
                &mut scratch,
                &defined,
                keys.as_deref(),
                comparator,
                fast_comparator.as_ref(),
            )?;
        }
        let mut sorted: Vec<Value> = order
            .into_iter()
            .map(|index| std::mem::replace(&mut defined[index], Value::Undefined))
            .collect();
        sorted.extend(undefined);
        Ok(sorted)
    }

    fn compare_elements(
        &mut self,
        left: usize,
        right: usize,
        values: &[Value],
        keys: Option<&[Vec<u16>]>,
        comparator: &Value,
        fast_comparator: Option<&Rc<super::vm::Closure>>,
    ) -> Result<Ordering, ScriptError> {
        if let Some(keys) = keys {
            return Ok(keys[left].cmp(&keys[right]));
        }
        let result = match fast_comparator {
            Some(closure) => self.call_simple_closure(
                closure,
                comparator,
                Value::Undefined,
                &[values[left].clone(), values[right].clone()],
            )?,
            None => self.call_function(
                comparator.clone(),
                Value::Undefined,
                vec![values[left].clone(), values[right].clone()],
            )?,
        };
        let number = match result {
            Value::Number(number) => number,
            other => self.coerce_number(other)?,
        };
        Ok(if number < 0.0 {
            Ordering::Less
        } else if number > 0.0 {
            Ordering::Greater
        } else {
            Ordering::Equal
        })
    }

    fn merge_sort(
        &mut self,
        order: &mut [usize],
        scratch: &mut [usize],
        values: &[Value],
        keys: Option<&[Vec<u16>]>,
        comparator: &Value,
        fast_comparator: Option<&Rc<super::vm::Closure>>,
    ) -> Result<(), ScriptError> {
        let length = order.len();
        if length <= 1 {
            return Ok(());
        }
        if length <= 12 {
            // insertion sort for short runs
            for i in 1..length {
                let mut j = i;
                while j > 0
                    && self.compare_elements(
                        order[j - 1],
                        order[j],
                        values,
                        keys,
                        comparator,
                        fast_comparator,
                    )? == Ordering::Greater
                {
                    order.swap(j - 1, j);
                    j -= 1;
                }
            }
            return Ok(());
        }
        let middle = length / 2;
        {
            let (left, right) = order.split_at_mut(middle);
            let (left_scratch, right_scratch) = scratch.split_at_mut(middle);
            self.merge_sort(
                left,
                left_scratch,
                values,
                keys,
                comparator,
                fast_comparator,
            )?;
            self.merge_sort(
                right,
                right_scratch,
                values,
                keys,
                comparator,
                fast_comparator,
            )?;
        }
        let (mut i, mut j, mut k) = (0, middle, 0);
        while i < middle && j < length {
            if self.compare_elements(
                order[i],
                order[j],
                values,
                keys,
                comparator,
                fast_comparator,
            )? == Ordering::Greater
            {
                scratch[k] = order[j];
                j += 1;
            } else {
                scratch[k] = order[i];
                i += 1;
            }
            k += 1;
        }
        while i < middle {
            scratch[k] = order[i];
            i += 1;
            k += 1;
        }
        while j < length {
            scratch[k] = order[j];
            j += 1;
            k += 1;
        }
        order.copy_from_slice(&scratch[..length]);
        Ok(())
    }
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn compare_elements_jit(
    session: &mut super::jit::JitCallbackSession<'_>,
    left: usize,
    right: usize,
    values: &[Value],
) -> Result<Ordering, ScriptError> {
    let result = match session.param_count() {
        0 => session.call0()?,
        1 => session.call1(values[left].clone())?,
        _ => session.call2(values[left].clone(), values[right].clone())?,
    };
    let number = match result {
        Value::Number(number) => number,
        other => session.with_runtime(|rt| rt.coerce_number(other))?,
    };
    Ok(if number < 0.0 {
        Ordering::Less
    } else if number > 0.0 {
        Ordering::Greater
    } else {
        Ordering::Equal
    })
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn merge_sort_jit(
    session: &mut super::jit::JitCallbackSession<'_>,
    order: &mut [usize],
    scratch: &mut [usize],
    values: &[Value],
) -> Result<(), ScriptError> {
    let length = order.len();
    if length <= 1 {
        return Ok(());
    }
    if length <= 12 {
        for i in 1..length {
            let mut j = i;
            while j > 0
                && compare_elements_jit(session, order[j - 1], order[j], values)?
                    == Ordering::Greater
            {
                order.swap(j - 1, j);
                j -= 1;
            }
        }
        return Ok(());
    }
    let middle = length / 2;
    {
        let (left, right) = order.split_at_mut(middle);
        let (left_scratch, right_scratch) = scratch.split_at_mut(middle);
        merge_sort_jit(session, left, left_scratch, values)?;
        merge_sort_jit(session, right, right_scratch, values)?;
    }
    let (mut i, mut j, mut k) = (0, middle, 0);
    while i < middle && j < length {
        if compare_elements_jit(session, order[i], order[j], values)? == Ordering::Greater {
            scratch[k] = order[j];
            j += 1;
        } else {
            scratch[k] = order[i];
            i += 1;
        }
        k += 1;
    }
    while i < middle {
        scratch[k] = order[i];
        i += 1;
        k += 1;
    }
    while j < length {
        scratch[k] = order[j];
        j += 1;
        k += 1;
    }
    order.copy_from_slice(&scratch[..length]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrays_take_subclass_prototypes_and_close_when_frozen() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                "use strict";
                const seen = [];
                class List extends Array { sum() { return this.reduce((a, b) => a + b, 0); } }
                const list = new List();
                list.push(1, 2, 3);
                seen.push(list instanceof List, Array.isArray(list), list.sum(), Object.getPrototypeOf(list) === List.prototype);
                const frozen = Object.freeze([1, 2]);
                seen.push(Object.isFrozen(frozen), Object.isExtensible(frozen), Object.getOwnPropertyDescriptor(frozen, 0).writable);
                for (const change of [() => frozen.push(3), () => { frozen[0] = 9; }, () => { frozen.length = 0; }, () => frozen.sort()]) {
                    try { change(); seen.push("changed"); } catch (error) { seen.push(error.name); }
                }
                seen.push(frozen.join(), Object.freeze([7]).reverse().join());
                const sealed = Object.seal([1, 2]);
                sealed[0] = 5;
                try { sealed.pop(); } catch (error) { seen.push(error.name); }
                try { delete sealed[1]; } catch (error) { seen.push(error.name); }
                const closed = Object.preventExtensions([1]);
                closed[0] = 2;
                try { closed.push(2); } catch (error) { seen.push(error.name); }
                seen.push(sealed.join(), closed.join(), Object.isSealed(sealed), Object.isFrozen(sealed));
                const plain = Object.setPrototypeOf([1], { hello() { return "hi"; } });
                seen.push(plain.hello(), Reflect.setPrototypeOf(frozen, null));
                const doubled = list.map((x) => x * 2);
                seen.push(doubled instanceof List, doubled.sum(), list.filter((x) => x > 1).length, list.slice(1) instanceof List, [1].map((x) => x) instanceof List, Array[Symbol.species] === Array);
                const lazy = [1, 2];
                let reads = 0;
                Object.defineProperty(lazy, 1, { get() { reads++; return 20; }, enumerable: true, configurable: true });
                Object.defineProperty(lazy, "total", { get() { return this[0] + this[1]; } });
                Object.defineProperty(lazy, 0, { value: 10, writable: false });
                try { lazy[0] = 5; } catch (error) { seen.push(error.name); }
                seen.push(lazy[1], lazy.total, lazy.map((x) => x).join(), Object.keys(lazy).join(), reads > 0, lazy.length);
                const fixed = [1, 2];
                Object.defineProperty(fixed, "length", { writable: false });
                try { fixed.push(3); } catch (error) { seen.push(error.name); }
                try { delete fixed.length; } catch (error) { seen.push(error.name); }
                document.getElementById("out").textContent = seen.join("|");
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(
                "true|true|6|true|true|false|false|TypeError|TypeError|TypeError|TypeError|1,2|7|\
                 TypeError|TypeError|TypeError|5,2|2|true|false|hi|false|true|12|2|true|false|true|TypeError|20|30|10,20|0,1|true|2|TypeError|TypeError"
            )
        );
    }

    #[test]
    fn array_prototypes_length_and_generic_methods_follow_spec() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                const proto = [10, 20];
                proto.tag = "p";
                const child = Object.create(proto);
                seen.push(Object.getPrototypeOf(child) === proto, child[1], child.tag, proto.isPrototypeOf(child));
                const keys = [];
                for (const k in proto) keys.push(k);
                seen.push(keys.join());
                const arr = [1, 2, 3];
                Object.defineProperty(arr, 1, { configurable: false });
                try { arr.length = -1; } catch (e) { seen.push(e.name); }
                arr.length = 0;
                seen.push(arr.length, arr[1]);
                const target = {};
                try {
                    Object.defineProperties(target, { a: { value: 1 }, b: 42 });
                } catch (e) {
                    seen.push(e.name, "a" in target);
                }
                const like = { length: 2, 0: "x", 1: "y" };
                const mapped = Array.prototype.map.call(like, (v, i, o) => {
                    if (i === 0) o[1] = "z";
                    return v + i;
                });
                seen.push(mapped.join());
                document.getElementById("out").textContent = seen.join("|");
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some("true|20|p|true|0,1,tag|RangeError|2|2|TypeError|false|x0,z1")
        );
    }

    #[test]
    fn array_unscopables_change_by_copy_and_species_follow_spec() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                const u = Array.prototype[Symbol.unscopables];
                seen.push(Object.getPrototypeOf(u), u.at, u.toReversed, u.toSorted, u.toSpliced, "with" in u);
                function Box(len) { this.createdWith = len; }
                const fromArr = Array.from.call(Box, { length: 2, 0: "a", 1: "b" });
                const ofArr = Array.of.call(Box, 7, 8);
                seen.push(fromArr.createdWith, fromArr[1], fromArr.length, ofArr.createdWith, ofArr[0], ofArr.length);
                const sparse = [1, , 3];
                const rev = sparse.toReversed();
                const spl = sparse.toSpliced(1, 1, 9);
                const w = sparse.with(1, 5);
                seen.push(1 in rev, rev.join("-"), 1 in spl, spl.join("-"), w.join("-"));
                let thrown = 0;
                const frozen = Object.freeze([]);
                for (const fn of [() => frozen.push(), () => frozen.pop(), () => frozen.shift(), () => frozen.unshift()]) {
                    try { fn(); } catch (e) { if (e instanceof TypeError) thrown++; }
                }
                seen.push(thrown);
                document.getElementById("out").textContent = seen.join("|");
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some("|true|true|true|true|false|2|b|2|2|7|2|true|3--1|true|1-9-3|1-5-3|4")
        );
    }
}
