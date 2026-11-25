// Array.prototype: methods run on engine arrays directly and read array-like receivers,
// such as arguments objects and node lists, through their indexed properties

use super::operations::{is_callable, same_value, same_value_zero, strict_equals};
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
        M::Push | M::Unshift => adding(arguments.len()).or_else(|| {
            (!arguments.is_empty() && matches!(method, M::Unshift))
                .then(changing)
                .flatten()
        }),
        M::Pop | M::Shift => removing(length),
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

    // ArraySpeciesCreate: an array made like the receiver, so a subclass's map or filter
    // hands back the subclass; a plain array, or a constructor without a species, gives one
    // of Array's own
    fn array_species(
        &mut self,
        original: &Value,
        values: Vec<Value>,
        set_length: bool,
    ) -> Result<Value, ScriptError> {
        let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
        let Value::Array(array) = original else {
            return Ok(Value::Array(heap.alloc_array_fast(ArrayData::from(values))));
        };
        if unsafe { heap.array_ref(array) }.is_some_and(|a| a.extra.is_none())
            && unsafe { heap.object_ref(&self.intrinsics.array_prototype) }
                .is_some_and(|p| p.array_constructor_intact(heap))
        {
            return Ok(Value::Array(heap.alloc_array_fast(ArrayData::from(values))));
        }
        let mut constructor = self.get_property(original, "constructor")?;
        if let Value::Object(object) = &constructor {
            if matches!(
                object.borrow().class,
                ObjectClass::Native(ref native) if matches!(**native, Native::ArrayConstructor)
            ) {
                return Ok(Self::new_array(values));
            }
            constructor = match self.get_property(&constructor, "@@species")? {
                Value::Null => Value::Undefined,
                species => species,
            };
        }
        match &constructor {
            Value::Undefined => return Ok(Self::new_array(values)),
            Value::Object(object)
                if matches!(
                    object.borrow().class,
                    ObjectClass::Native(ref native) if matches!(**native, Native::ArrayConstructor)
                ) =>
            {
                return Ok(Self::new_array(values));
            }
            _ => {}
        }
        if !Self::is_constructor(&constructor) {
            return Err(self.type_error("object.constructor[Symbol.species] is not a constructor"));
        }
        let made = self.construct(constructor, vec![Value::Number(0.0)], None)?;
        let length = values.len();
        // CreateDataPropertyOrThrow defines each element, over whatever the constructor put there
        for (index, value) in values.into_iter().enumerate() {
            if matches!(value, Value::Uninitialized) {
                continue;
            }
            let descriptor = self.object_from_entries(vec![
                ("value".to_string(), value),
                ("writable".to_string(), Value::Bool(true)),
                ("enumerable".to_string(), Value::Bool(true)),
                ("configurable".to_string(), Value::Bool(true)),
            ]);
            self.define_property_value(&made, index.to_string(), &descriptor)?;
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

    // a callback method's receiver coerced once: its elements, and the object the
    // callback's last argument sees, which is the one the length was read from
    fn callback_receiver(
        &mut self,
        this: &Value,
        method: &str,
    ) -> Result<(Vec<Value>, Value), ScriptError> {
        let object = match this {
            Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Symbol(_)
            | Value::BigInt(_) => self.coerce_object(this.clone())?,
            _ => this.clone(),
        };
        // a string's wrapper has no observable length lookup, so its characters
        // are read straight from the primitive
        let source = if matches!(this, Value::String(_)) {
            this
        } else {
            &object
        };
        Ok((self.receiver_values(source, method)?, object))
    }

    // a mutating method on a generic receiver such as a Proxy over an array: push and pop
    // follow the standard's own steps, and the rest run on a copy whose changes are written
    // back through the receiver's properties
    fn mutate_array_like(
        &mut self,
        method: ArrayMethod,
        this: Value,
        arguments: Vec<Value>,
        name: &str,
    ) -> Result<Value, ScriptError> {
        if matches!(this, Value::Undefined | Value::Null) {
            return Err(self.type_error(format!(
                "Array.prototype.{name} called on null or undefined"
            )));
        }
        // sort rejects a comparator that is not callable before it reads anything
        if matches!(method, ArrayMethod::Sort) {
            let comparator = argument(&arguments, 0);
            if !matches!(comparator, Value::Undefined) && !is_callable(&comparator) {
                return Err(self
                    .type_error("The comparison function must be either a function or undefined"));
            }
        }
        let length_value = self.get_property(&this, "length")?;
        let length = self.coerce_number(length_value)?;
        let length = if length.is_nan() || length <= 0.0 {
            0
        } else {
            length.min(MAX_RUNTIME_ARRAY_LENGTH as f64) as usize
        };
        match method {
            ArrayMethod::Push => {
                let new_length = length + arguments.len();
                check_length(new_length)?;
                for (offset, value) in arguments.into_iter().enumerate() {
                    self.set_property(&this, &(length + offset).to_string(), value, true)?;
                }
                let new_length = Value::Number(new_length as f64);
                self.set_property(&this, "length", new_length.clone(), true)?;
                return Ok(new_length);
            }
            ArrayMethod::Pop => {
                if length == 0 {
                    self.set_property(&this, "length", Value::Number(0.0), true)?;
                    return Ok(Value::Undefined);
                }
                let key = (length - 1).to_string();
                let value = self.get_property(&this, &key)?;
                self.delete_property(&this, &key, true)?;
                self.set_property(&this, "length", Value::Number((length - 1) as f64), true)?;
                return Ok(value);
            }
            ArrayMethod::CopyWithin => {
                let target = self.integer(argument(&arguments, 0), 0.0)?;
                let start = self.integer(argument(&arguments, 1), 0.0)?;
                let end = match argument(&arguments, 2) {
                    Value::Undefined => length as f64,
                    value => self.integer(value, 0.0)?,
                };
                let (to, from, last) = (
                    relative(target, length),
                    relative(start, length),
                    relative(end, length),
                );
                let count = last.saturating_sub(from).min(length - to);
                let order: Vec<usize> = if from < to && to < from + count {
                    (0..count).rev().collect()
                } else {
                    (0..count).collect()
                };
                for offset in order {
                    let source = (from + offset).to_string();
                    let destination = (to + offset).to_string();
                    if self.has_property(&this, &source)? {
                        let value = self.get_property(&this, &source)?;
                        self.set_property(&this, &destination, value, true)?;
                    } else {
                        self.delete_property(&this, &destination, true)?;
                    }
                }
                return Ok(this);
            }
            _ => {}
        }
        let before = self.array_like_list(&this)?;
        let copy = Self::new_array(before.clone());
        let result = self.call_array_method(method, copy.clone(), arguments)?;
        let after = match &copy {
            Value::Array(array) => array.borrow().clone(),
            _ => unreachable!("the copy is an array"),
        };
        for (index, value) in after.iter().enumerate() {
            if before
                .get(index)
                .is_none_or(|previous| !same_value(previous, value))
            {
                self.set_property(&this, &index.to_string(), value.clone(), true)?;
            }
        }
        for index in after.len()..before.len() {
            self.delete_property(&this, &index.to_string(), true)?;
        }
        // the methods that may change the length always write it, as the standard's steps do
        if matches!(
            method,
            ArrayMethod::Shift | ArrayMethod::Unshift | ArrayMethod::Splice
        ) {
            self.set_property(&this, "length", Value::Number(after.len() as f64), true)?;
        }
        Ok(if values_equal(&result, &copy) {
            this
        } else {
            result
        })
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
                let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
                let (values, object, length) = if let Value::Array(array) = &this {
                    let len = unsafe { heap.array_ref(array) }
                        .map_or_else(|| array.borrow().len(), |a| a.len());
                    (Vec::new(), this.clone(), len)
                } else {
                    let (values, object) = self.callback_receiver(&this, name)?;
                    let len = values.len();
                    (values, object, len)
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
                                        break;
                                    }
                                    if param_count == 0
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
                                        break;
                                    }
                                    borrowed[index].clone()
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
                        M::Map | M::Filter => self.array_species(&this, mapped, false)?,
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
                                    break;
                                }
                                data.values[index].clone()
                            }
                            None => {
                                let borrowed = array.borrow();
                                if index >= borrowed.len() {
                                    break;
                                }
                                borrowed[index].clone()
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
                    M::Map | M::Filter => self.array_species(&this, mapped, false)?,
                    M::Some => Value::Bool(false),
                    M::Every => Value::Bool(true),
                    M::FindIndex => Value::Number(-1.0),
                    _ => Value::Undefined,
                })
            }
            M::Reduce | M::ReduceRight => {
                let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
                let (values, object, length) = if let Value::Array(array) = &this {
                    let len = unsafe { heap.array_ref(array) }
                        .map_or_else(|| array.borrow().len(), |a| a.len());
                    (Vec::new(), this.clone(), len)
                } else {
                    let (values, object) = self.callback_receiver(&this, name)?;
                    let len = values.len();
                    (values, object, len)
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
                    accumulator = match Self::element(&this, &values, first) {
                        Some(Value::Uninitialized) if !self.holds(&this, &first.to_string()) => {
                            None
                        }
                        Some(Value::Uninitialized) => {
                            Some(self.get_index(&this, Value::Number(first as f64))?)
                        }
                        value => value,
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
                                Some(data) => {
                                    data.values.get(index).cloned().unwrap_or(Value::Undefined)
                                }
                                None => array
                                    .borrow()
                                    .get(index)
                                    .cloned()
                                    .unwrap_or(Value::Undefined),
                            }
                        } else {
                            Self::element(&this, &values, index).unwrap_or(Value::Undefined)
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
                    let value = Self::element(&this, &values, index).unwrap_or(Value::Undefined);
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
            M::Sort | M::ToSorted => {
                let comparator = argument(arguments, 0);
                if !matches!(comparator, Value::Undefined) && !is_callable(&comparator) {
                    return Err(self.type_error(
                        "The comparison function must be either a function or undefined",
                    ));
                }
                let values = if matches!(method, M::Sort) {
                    self.receiver_array(&this, name)?.borrow().to_vec()
                } else {
                    self.receiver_values(&this, name)?
                };
                let sorted = self.sort_values(values, &comparator)?;
                if matches!(method, M::Sort) {
                    let array = self.receiver_array(&this, name)?;
                    **array.borrow_mut() = sorted;
                    Ok(this)
                } else {
                    Ok(Self::new_array(sorted))
                }
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
        if mutates && !matches!(this, Value::Array(_)) {
            return self.mutate_array_like(method, this, arguments, name);
        }
        if mutates
            && let Value::Array(array) = &this
            && let Some(refusal) = array_method_refusal(method, &array.borrow(), &arguments)
        {
            return Err(self.type_error(refusal));
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
                let (values, object, length) = if let Value::Array(array) = &this {
                    (Vec::new(), this.clone(), array.borrow().len())
                } else {
                    let (values, object) = self.callback_receiver(&this, name)?;
                    let len = values.len();
                    (values, object, len)
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
                let mut values = self.receiver_values(&this, name)?;
                // an index nothing was put at is searched as what a prototype has there
                for (index, value) in values.iter_mut().enumerate() {
                    if matches!(value, Value::Uninitialized)
                        && self.holds(&this, &index.to_string())
                    {
                        *value = self.get_index(&this, Value::Number(index as f64))?;
                    }
                }
                let target = argument(&arguments, 0);
                let length = values.len();
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
                        if strict_equals(&values[index], &target) {
                            return Ok(Value::Number(index as f64));
                        }
                    }
                    return Ok(Value::Number(-1.0));
                }
                let from = self.integer(argument(&arguments, 1), 0.0)?;
                let start = relative(from, length);
                for (index, value) in values.iter().enumerate().skip(start) {
                    let found = match method {
                        M::Includes => same_value_zero(&value.clone().filled(), &target),
                        _ => strict_equals(value, &target),
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
            M::Join | M::ToString | M::ToLocaleString => {
                if matches!(method, M::ToString) && !matches!(this, Value::Array(_)) {
                    return Ok(Value::String(self.object_to_string(&this)?.into()));
                }
                let separator = match (method, argument(&arguments, 0)) {
                    (M::Join, Value::Undefined) | (M::ToString | M::ToLocaleString, _) => {
                        ",".to_string()
                    }
                    (_, separator) => self.coerce_string(separator)?,
                };
                match &this {
                    Value::Array(array) => {
                        let array = array.clone();
                        Ok(Value::String(self.join_array(&array, &separator)?.into()))
                    }
                    _ => {
                        let values = self.receiver_values(&this, name)?;
                        let array = super::heap::alloc_array(values.into());
                        Ok(Value::String(self.join_array(&array, &separator)?.into()))
                    }
                }
            }
            M::At => {
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
                let mut result = self.receiver_values(&this, name)?;
                for value in arguments {
                    match value {
                        Value::Array(array) => result.extend(array.borrow().iter().cloned()),
                        value => result.push(value),
                    }
                    check_length(result.len())?;
                }
                self.array_species(&this, result, true)
            }
            M::Slice => {
                let values = self.receiver_values(&this, name)?;
                let length = values.len();
                let start = self.integer(argument(&arguments, 0), 0.0)?;
                let end = self.integer(argument(&arguments, 1), length as f64)?;
                let start = relative(start, length);
                let end = relative(end, length);
                let picked = if start < end {
                    values[start..end].to_vec()
                } else {
                    Vec::new()
                };
                self.array_species(&this, picked, true)
            }
            M::Flat | M::FlatMap => {
                let (values, object) = self.callback_receiver(&this, name)?;
                let (depth, mapper) = if matches!(method, M::FlatMap) {
                    (1.0, Some(self.callback(&arguments, name)?))
                } else {
                    (self.integer(argument(&arguments, 0), 1.0)?, None)
                };
                let mut result = Vec::new();
                for (index, value) in values.into_iter().enumerate() {
                    let value = match &mapper {
                        Some(mapper) => self.call_function(
                            mapper.clone(),
                            argument(&arguments, 1),
                            vec![value, Value::Number(index as f64), object.clone()],
                        )?,
                        None => value,
                    };
                    flatten_into(&mut result, value, depth)?;
                }
                self.array_species(&this, result, false)
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
                Ok(value.unwrap_or(Value::Undefined))
            }
            M::Shift => {
                let array = self.receiver_array(&this, name)?;
                let mut array = array.borrow_mut();
                Ok(if array.is_empty() {
                    Value::Undefined
                } else {
                    array.remove(0)
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
                let mut values = self.receiver_values(&this, name)?;
                values.reverse();
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
                let length = array.borrow().len();
                let target = self.integer(argument(&arguments, 0), 0.0)?;
                let start = self.integer(argument(&arguments, 1), 0.0)?;
                let end = self.integer(argument(&arguments, 2), length as f64)?;
                let (target, start, end) = (
                    relative(target, length),
                    relative(start, length),
                    relative(end, length),
                );
                let count = end.saturating_sub(start).min(length - target);
                // the coercions above may have shrunk the array, so read what is there now
                let mut values = array.borrow_mut();
                let copy = (start..start + count)
                    .map(|from| values.get(from).cloned())
                    .collect::<Vec<_>>();
                for (offset, value) in copy.into_iter().enumerate() {
                    let to = target + offset;
                    match value {
                        Some(value) if to < values.len() => values[to] = value,
                        Some(value) => {
                            values.resize(to, Value::Undefined);
                            values.push(value);
                        }
                        None if to < values.len() => values[to] = Value::Undefined,
                        None => {}
                    }
                }
                drop(values);
                Ok(this)
            }
            M::Splice | M::ToSpliced => {
                let mut values = if matches!(method, M::Splice) {
                    self.receiver_array(&this, name)?.borrow().to_vec()
                } else {
                    self.receiver_values(&this, name)?
                };
                let length = values.len();
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
                let insert: Vec<Value> = arguments.iter().skip(2).cloned().collect();
                check_length(length - delete + insert.len())?;
                let removed: Vec<Value> = values.splice(start..start + delete, insert).collect();
                if matches!(method, M::Splice) {
                    let array = self.receiver_array(&this, name)?;
                    **array.borrow_mut() = values;
                    self.array_species(&this, removed, true)
                } else {
                    Ok(Self::new_array(values))
                }
            }
            M::With => {
                let mut values = self.receiver_values(&this, name)?;
                let index = self.integer(argument(&arguments, 0), 0.0)?;
                let index = if index < 0.0 {
                    values.len() as f64 + index
                } else {
                    index
                };
                if index < 0.0 || index >= values.len() as f64 {
                    return Err(self.range_error("Invalid index"));
                }
                values[index as usize] = argument(&arguments, 1);
                Ok(Self::new_array(values))
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

fn flatten_into(result: &mut Vec<Value>, value: Value, depth: f64) -> Result<(), ScriptError> {
    match value {
        Value::Array(array) if depth >= 1.0 => {
            let items = array.borrow().clone();
            for item in items {
                flatten_into(result, item, depth - 1.0)?;
            }
        }
        value => {
            result.push(value);
            check_length(result.len())?;
        }
    }
    Ok(())
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
}
