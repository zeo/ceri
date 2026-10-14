// ArrayBuffer, the typed array views over it and DataView: views share one byte buffer and
// read and write elements in little-endian order with the spec's numeric conversions

use super::arrays::ArrayMethod;
use super::object::{JsObject, ObjectClass, new_object};
use super::operations::is_callable;
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ElementKind {
    Int8,
    Uint8,
    Uint8Clamped,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float32,
    Float64,
}

pub(super) const ELEMENT_KINDS: &[(ElementKind, &str)] = &[
    (ElementKind::Int8, "Int8Array"),
    (ElementKind::Uint8, "Uint8Array"),
    (ElementKind::Uint8Clamped, "Uint8ClampedArray"),
    (ElementKind::Int16, "Int16Array"),
    (ElementKind::Uint16, "Uint16Array"),
    (ElementKind::Int32, "Int32Array"),
    (ElementKind::Uint32, "Uint32Array"),
    (ElementKind::Float32, "Float32Array"),
    (ElementKind::Float64, "Float64Array"),
];

impl ElementKind {
    pub(super) fn size(self) -> usize {
        match self {
            ElementKind::Int8 | ElementKind::Uint8 | ElementKind::Uint8Clamped => 1,
            ElementKind::Int16 | ElementKind::Uint16 => 2,
            ElementKind::Int32 | ElementKind::Uint32 | ElementKind::Float32 => 4,
            ElementKind::Float64 => 8,
        }
    }

    pub(super) fn name(self) -> &'static str {
        ELEMENT_KINDS
            .iter()
            .find(|(kind, _)| *kind == self)
            .map_or("TypedArray", |(_, name)| name)
    }

    pub(super) fn read(self, bytes: &[u8]) -> f64 {
        match self {
            ElementKind::Int8 => f64::from(bytes[0] as i8),
            ElementKind::Uint8 | ElementKind::Uint8Clamped => f64::from(bytes[0]),
            ElementKind::Int16 => f64::from(i16::from_le_bytes([bytes[0], bytes[1]])),
            ElementKind::Uint16 => f64::from(u16::from_le_bytes([bytes[0], bytes[1]])),
            ElementKind::Int32 => f64::from(i32::from_le_bytes(
                bytes[..4].try_into().expect("four bytes"),
            )),
            ElementKind::Uint32 => f64::from(u32::from_le_bytes(
                bytes[..4].try_into().expect("four bytes"),
            )),
            ElementKind::Float32 => f64::from(f32::from_le_bytes(
                bytes[..4].try_into().expect("four bytes"),
            )),
            ElementKind::Float64 => f64::from_le_bytes(bytes[..8].try_into().expect("eight bytes")),
        }
    }

    // the element bytes for a number, with ToInt8 and friends or clamping
    pub(super) fn encode(self, number: f64) -> Vec<u8> {
        let integer = |number: f64| -> i64 {
            if number.is_finite() {
                (number.trunc() as i128).rem_euclid(1 << 32) as i64
            } else {
                0
            }
        };
        match self {
            ElementKind::Int8 | ElementKind::Uint8 => vec![integer(number) as u8],
            ElementKind::Uint8Clamped => {
                let value = if number.is_nan() {
                    0.0
                } else {
                    // round half to even, then clamp
                    let clamped = number.clamp(0.0, 255.0);
                    let floor = clamped.floor();
                    let difference = clamped - floor;
                    if difference > 0.5 || (difference == 0.5 && floor % 2.0 != 0.0) {
                        floor + 1.0
                    } else {
                        floor
                    }
                };
                vec![value as u8]
            }
            ElementKind::Int16 | ElementKind::Uint16 => {
                (integer(number) as u16).to_le_bytes().to_vec()
            }
            ElementKind::Int32 | ElementKind::Uint32 => {
                (integer(number) as u32).to_le_bytes().to_vec()
            }
            ElementKind::Float32 => (number as f32).to_le_bytes().to_vec(),
            ElementKind::Float64 => number.to_le_bytes().to_vec(),
        }
    }
}

pub(super) fn element_count(array: &TypedArrayData) -> usize {
    (array.end - array.start) / array.kind.size()
}

pub(super) fn read_element(array: &TypedArrayData, index: usize) -> Option<f64> {
    if index >= element_count(array) {
        return None;
    }
    let size = array.kind.size();
    let offset = array.start + index * size;
    let buffer = array.buffer.borrow();
    buffer
        .get(offset..offset + size)
        .map(|bytes| array.kind.read(bytes))
}

pub(super) fn write_element(array: &TypedArrayData, index: usize, number: f64) {
    if index >= element_count(array) {
        return;
    }
    let size = array.kind.size();
    let offset = array.start + index * size;
    let bytes = array.kind.encode(number);
    if let Some(slot) = array.buffer.borrow_mut().get_mut(offset..offset + size) {
        slot.copy_from_slice(&bytes);
    }
}

pub(super) fn elements(array: &TypedArrayData) -> Vec<Value> {
    (0..element_count(array))
        .filter_map(|index| read_element(array, index))
        .map(Value::Number)
        .collect()
}

pub(super) fn new_typed(kind: ElementKind, buffer: BytesHandle, start: usize, end: usize) -> Value {
    Value::TypedArray(Rc::new(RefCell::new(TypedArrayData {
        buffer,
        start,
        end,
        kind,
    })))
}

#[derive(Clone, Copy, Debug)]
pub(super) enum TypedMethod {
    Constructor(ElementKind),
    AbstractConstructor,
    From,
    Of,
    Buffer,
    ByteLength,
    ByteOffset,
    Length,
    Set,
    Subarray,
    Values,
    Keys,
    Entries,
    ToStringTag,
    // array algorithms run over a copy of the elements
    Read(ArrayMethod),
    // array algorithms whose result becomes a typed array of the same kind
    Derive(ArrayMethod),
    // array algorithms that rewrite the elements in place
    Mutate(ArrayMethod),
    BufferConstructor,
    BufferIsView,
    BufferByteLength,
    BufferSlice,
    ViewConstructor,
    ViewBuffer,
    ViewByteLength,
    ViewByteOffset,
    ViewGet(ElementKind),
    ViewSet(ElementKind),
}

pub(super) const TYPED_METHODS: &[(&str, TypedMethod, u32)] = &[
    ("set", TypedMethod::Set, 1),
    ("subarray", TypedMethod::Subarray, 2),
    ("keys", TypedMethod::Keys, 0),
    ("entries", TypedMethod::Entries, 0),
    ("at", TypedMethod::Read(ArrayMethod::At), 1),
    ("every", TypedMethod::Read(ArrayMethod::Every), 1),
    ("find", TypedMethod::Read(ArrayMethod::Find), 1),
    ("findIndex", TypedMethod::Read(ArrayMethod::FindIndex), 1),
    ("findLast", TypedMethod::Read(ArrayMethod::FindLast), 1),
    (
        "findLastIndex",
        TypedMethod::Read(ArrayMethod::FindLastIndex),
        1,
    ),
    ("forEach", TypedMethod::Read(ArrayMethod::ForEach), 1),
    ("includes", TypedMethod::Read(ArrayMethod::Includes), 1),
    ("indexOf", TypedMethod::Read(ArrayMethod::IndexOf), 1),
    ("join", TypedMethod::Read(ArrayMethod::Join), 1),
    (
        "lastIndexOf",
        TypedMethod::Read(ArrayMethod::LastIndexOf),
        1,
    ),
    ("reduce", TypedMethod::Read(ArrayMethod::Reduce), 1),
    (
        "reduceRight",
        TypedMethod::Read(ArrayMethod::ReduceRight),
        1,
    ),
    ("some", TypedMethod::Read(ArrayMethod::Some), 1),
    (
        "toLocaleString",
        TypedMethod::Read(ArrayMethod::ToLocaleString),
        0,
    ),
    ("toString", TypedMethod::Read(ArrayMethod::ToString), 0),
    ("filter", TypedMethod::Derive(ArrayMethod::Filter), 1),
    ("map", TypedMethod::Derive(ArrayMethod::Map), 1),
    ("slice", TypedMethod::Derive(ArrayMethod::Slice), 2),
    (
        "toReversed",
        TypedMethod::Derive(ArrayMethod::ToReversed),
        0,
    ),
    ("toSorted", TypedMethod::Derive(ArrayMethod::ToSorted), 1),
    ("with", TypedMethod::Derive(ArrayMethod::With), 2),
    (
        "copyWithin",
        TypedMethod::Mutate(ArrayMethod::CopyWithin),
        2,
    ),
    ("fill", TypedMethod::Mutate(ArrayMethod::Fill), 1),
    ("reverse", TypedMethod::Mutate(ArrayMethod::Reverse), 0),
    ("sort", TypedMethod::Mutate(ArrayMethod::Sort), 1),
];

pub(super) const VIEW_ACCESSORS: &[(&str, ElementKind)] = &[
    ("Int8", ElementKind::Int8),
    ("Uint8", ElementKind::Uint8),
    ("Int16", ElementKind::Int16),
    ("Uint16", ElementKind::Uint16),
    ("Int32", ElementKind::Int32),
    ("Uint32", ElementKind::Uint32),
    ("Float32", ElementKind::Float32),
    ("Float64", ElementKind::Float64),
];

fn argument(arguments: &[Value], index: usize) -> Value {
    arguments.get(index).cloned().unwrap_or(Value::Undefined)
}

// a relative offset argument clamped into 0..=length
fn relative(value: f64, length: usize) -> usize {
    let length = length as f64;
    (if value < 0.0 {
        (length + value).max(0.0)
    } else {
        value.min(length)
    }) as usize
}

impl ScriptRuntime {
    fn this_typed(&mut self, this: &Value) -> Result<TypedArrayHandle, ScriptError> {
        match this {
            Value::TypedArray(array) => Ok(array.clone()),
            _ => Err(self.type_error("this is not a typed array")),
        }
    }

    fn index_argument(&mut self, value: Value, label: &str) -> Result<usize, ScriptError> {
        let number = match value {
            Value::Undefined => 0.0,
            other => self.coerce_number(other)?,
        };
        let number = if number.is_nan() { 0.0 } else { number.trunc() };
        if number < 0.0 || number > MAX_TYPED_ARRAY_BYTES as f64 {
            return Err(self.range_error(format!("Invalid {label}")));
        }
        Ok(number as usize)
    }

    fn allocate(&mut self, bytes: usize) -> Result<BytesHandle, ScriptError> {
        if bytes > MAX_TYPED_ARRAY_BYTES {
            return Err(self.range_error("Array buffer allocation failed"));
        }
        Ok(Rc::new(RefCell::new(vec![0; bytes])))
    }

    // a typed array of a kind filled from a list of numbers
    fn typed_from_values(
        &mut self,
        kind: ElementKind,
        values: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let buffer = self.allocate(values.len() * kind.size())?;
        let length = values.len() * kind.size();
        let array = TypedArrayData {
            buffer,
            start: 0,
            end: length,
            kind,
        };
        for (index, value) in values.into_iter().enumerate() {
            let number = self.coerce_number(value)?;
            write_element(&array, index, number);
        }
        Ok(Value::TypedArray(Rc::new(RefCell::new(array))))
    }

    pub(super) fn typed_get(&self, array: &TypedArrayHandle, key: &str) -> Option<Value> {
        let index = super::object::array_index(key)?;
        Some(read_element(&array.borrow(), index as usize).map_or(Value::Undefined, Value::Number))
    }

    pub(super) fn typed_set(
        &mut self,
        array: &TypedArrayHandle,
        index: usize,
        value: Value,
    ) -> Result<(), ScriptError> {
        let number = self.coerce_number(value)?;
        write_element(&array.borrow(), index, number);
        Ok(())
    }

    pub(super) fn call_typed(
        &mut self,
        method: TypedMethod,
        this: Value,
        arguments: Vec<Value>,
        new_target: Option<Value>,
    ) -> Result<Value, ScriptError> {
        use TypedMethod as M;
        let first = argument(&arguments, 0);
        match method {
            M::Constructor(kind) => {
                if new_target.is_none() {
                    return Err(
                        self.type_error(format!("Constructor {} requires 'new'", kind.name()))
                    );
                }
                match &first {
                    Value::Bytes(buffer) => {
                        let buffer_length = buffer.borrow().len();
                        let offset =
                            self.index_argument(argument(&arguments, 1), "typed array offset")?;
                        if offset % kind.size() != 0 {
                            return Err(self.range_error(format!(
                                "start offset of {} should be a multiple of {}",
                                kind.name(),
                                kind.size()
                            )));
                        }
                        let end = match argument(&arguments, 2) {
                            Value::Undefined => {
                                if (buffer_length - offset.min(buffer_length)) % kind.size() != 0 {
                                    return Err(self.range_error(format!(
                                        "byte length of {} should be a multiple of {}",
                                        kind.name(),
                                        kind.size()
                                    )));
                                }
                                buffer_length
                            }
                            length => {
                                offset
                                    + self.index_argument(length, "typed array length")?
                                        * kind.size()
                            }
                        };
                        if offset > buffer_length || end > buffer_length {
                            return Err(self.range_error("Invalid typed array length"));
                        }
                        Ok(new_typed(kind, buffer.clone(), offset, end))
                    }
                    Value::TypedArray(source) => {
                        let values = elements(&source.borrow());
                        self.typed_from_values(kind, values)
                    }
                    Value::Object(_) | Value::Array(_) => {
                        let iterable = !matches!(
                            self.get_property(&first, "@@iterator")?,
                            Value::Undefined | Value::Null
                        );
                        let values = if iterable {
                            self.iterate_to_vec(first)?
                        } else {
                            self.array_like_list(&first)?
                        };
                        self.typed_from_values(kind, values)
                    }
                    _ => {
                        let length = self.index_argument(first, "typed array length")?;
                        let buffer = self.allocate(length * kind.size())?;
                        Ok(new_typed(kind, buffer, 0, length * kind.size()))
                    }
                }
            }
            M::AbstractConstructor => {
                Err(self.type_error("Abstract class TypedArray not directly constructable"))
            }
            M::From | M::Of => {
                let kind = self.kind_of_constructor(&this)?;
                let values = if matches!(method, M::Of) {
                    arguments
                } else {
                    let mapper = argument(&arguments, 1);
                    let values = self.iterate_to_vec(first)?;
                    if is_callable(&mapper) {
                        let mut mapped = Vec::with_capacity(values.len());
                        for (index, value) in values.into_iter().enumerate() {
                            mapped.push(self.call_function(
                                mapper.clone(),
                                argument(&arguments, 2),
                                vec![value, Value::Number(index as f64)],
                            )?);
                        }
                        mapped
                    } else {
                        values
                    }
                };
                self.typed_from_values(kind, values)
            }
            M::Buffer => Ok(Value::Bytes(
                self.this_typed(&this)?.borrow().buffer.clone(),
            )),
            M::ByteLength => {
                let array = self.this_typed(&this)?;
                let array = array.borrow();
                Ok(Value::Number((array.end - array.start) as f64))
            }
            M::ByteOffset => Ok(Value::Number(self.this_typed(&this)?.borrow().start as f64)),
            M::Length => Ok(Value::Number(
                element_count(&self.this_typed(&this)?.borrow()) as f64,
            )),
            M::ToStringTag => Ok(match &this {
                Value::TypedArray(array) => Value::String(array.borrow().kind.name().into()),
                _ => Value::Undefined,
            }),
            M::Set => {
                let array = self.this_typed(&this)?;
                let offset = self.index_argument(argument(&arguments, 1), "offset")?;
                let values = match &first {
                    Value::TypedArray(source) => elements(&source.borrow()),
                    other => self.array_like_list(other)?,
                };
                let count = element_count(&array.borrow());
                if offset + values.len() > count {
                    return Err(self.range_error("offset is out of bounds"));
                }
                for (index, value) in values.into_iter().enumerate() {
                    self.typed_set(&array, offset + index, value)?;
                }
                Ok(Value::Undefined)
            }
            M::Subarray => {
                let array = self.this_typed(&this)?;
                let (kind, buffer, start, count) = {
                    let array = array.borrow();
                    (
                        array.kind,
                        array.buffer.clone(),
                        array.start,
                        element_count(&array),
                    )
                };
                let begin = match first {
                    Value::Undefined => 0.0,
                    value => self.coerce_number(value)?.trunc(),
                };
                let end = match argument(&arguments, 1) {
                    Value::Undefined => count as f64,
                    value => self.coerce_number(value)?.trunc(),
                };
                let begin = relative(if begin.is_nan() { 0.0 } else { begin }, count);
                let end = relative(if end.is_nan() { 0.0 } else { end }, count).max(begin);
                Ok(new_typed(
                    kind,
                    buffer,
                    start + begin * kind.size(),
                    start + end * kind.size(),
                ))
            }
            M::Values | M::Keys | M::Entries => {
                let array = self.this_typed(&this)?;
                let values = elements(&array.borrow());
                let values = match method {
                    M::Values => values,
                    M::Keys => (0..values.len())
                        .map(|index| Value::Number(index as f64))
                        .collect(),
                    _ => values
                        .into_iter()
                        .enumerate()
                        .map(|(index, value)| {
                            Self::new_array(vec![Value::Number(index as f64), value])
                        })
                        .collect(),
                };
                Ok(Value::Iterator(Rc::new(RefCell::new(IteratorData {
                    values,
                    index: 0,
                }))))
            }
            M::Read(array_method) => {
                let array = self.this_typed(&this)?;
                let copy = Self::new_array(elements(&array.borrow()));
                self.call_array_method(array_method, copy, arguments)
            }
            M::Derive(array_method) => {
                let array = self.this_typed(&this)?;
                let kind = array.borrow().kind;
                let copy = Self::new_array(elements(&array.borrow()));
                // numbers sort numerically in typed arrays
                let arguments = if matches!(array_method, ArrayMethod::ToSorted)
                    && matches!(first, Value::Undefined)
                {
                    vec![self.numeric_comparator()]
                } else {
                    arguments
                };
                let result = self.call_array_method(array_method, copy, arguments)?;
                let values = match result {
                    Value::Array(values) => values.borrow().clone(),
                    other => vec![other],
                };
                self.typed_from_values(kind, values)
            }
            M::Mutate(array_method) => {
                let array = self.this_typed(&this)?;
                let copy = Self::new_array(elements(&array.borrow()));
                let arguments = if matches!(array_method, ArrayMethod::Sort)
                    && matches!(first, Value::Undefined)
                {
                    vec![self.numeric_comparator()]
                } else {
                    arguments
                };
                self.call_array_method(array_method, copy.clone(), arguments)?;
                if let Value::Array(values) = copy {
                    let values = values.borrow().clone();
                    for (index, value) in values.into_iter().enumerate() {
                        self.typed_set(&array, index, value)?;
                    }
                }
                Ok(this)
            }
            M::BufferConstructor => {
                if new_target.is_none() {
                    return Err(self.type_error("Constructor ArrayBuffer requires 'new'"));
                }
                let length = self.index_argument(first, "array buffer length")?;
                Ok(Value::Bytes(self.allocate(length)?))
            }
            M::BufferIsView => Ok(Value::Bool(
                matches!(first, Value::TypedArray(_))
                    || matches!(&first, Value::Object(object) if matches!(object.borrow().class, ObjectClass::DataView { .. })),
            )),
            M::BufferByteLength => match &this {
                Value::Bytes(bytes) => Ok(Value::Number(bytes.borrow().len() as f64)),
                _ => Err(self.type_error("this is not an ArrayBuffer")),
            },
            M::BufferSlice => {
                let Value::Bytes(bytes) = &this else {
                    return Err(self.type_error("this is not an ArrayBuffer"));
                };
                let length = bytes.borrow().len();
                let start = match first {
                    Value::Undefined => 0.0,
                    value => self.coerce_number(value)?.trunc(),
                };
                let end = match argument(&arguments, 1) {
                    Value::Undefined => length as f64,
                    value => self.coerce_number(value)?.trunc(),
                };
                let start = relative(if start.is_nan() { 0.0 } else { start }, length);
                let end = relative(if end.is_nan() { 0.0 } else { end }, length).max(start);
                Ok(Value::Bytes(Rc::new(RefCell::new(
                    bytes.borrow()[start..end].to_vec(),
                ))))
            }
            M::ViewConstructor => {
                if new_target.is_none() {
                    return Err(self.type_error("Constructor DataView requires 'new'"));
                }
                let Value::Bytes(buffer) = &first else {
                    return Err(self.type_error(
                        "First argument to DataView constructor must be an ArrayBuffer",
                    ));
                };
                let length = buffer.borrow().len();
                let offset = self.index_argument(argument(&arguments, 1), "DataView offset")?;
                let end = match argument(&arguments, 2) {
                    Value::Undefined => length,
                    value => offset + self.index_argument(value, "DataView length")?,
                };
                if offset > length || end > length {
                    return Err(self.range_error("Invalid DataView length"));
                }
                let prototype = self.intrinsics.data_view_prototype.clone();
                Ok(Value::Object(new_object(JsObject::with_prototype(
                    Some(prototype),
                    ObjectClass::DataView {
                        buffer: buffer.clone(),
                        start: offset,
                        end,
                    },
                ))))
            }
            M::ViewBuffer
            | M::ViewByteLength
            | M::ViewByteOffset
            | M::ViewGet(_)
            | M::ViewSet(_) => {
                let view = match &this {
                    Value::Object(object) => match &object.borrow().class {
                        ObjectClass::DataView { buffer, start, end } => {
                            Some((buffer.clone(), *start, *end))
                        }
                        _ => None,
                    },
                    _ => None,
                };
                let Some((buffer, start, end)) = view else {
                    return Err(self.type_error("this is not a DataView"));
                };
                match method {
                    M::ViewBuffer => Ok(Value::Bytes(buffer)),
                    M::ViewByteLength => Ok(Value::Number((end - start) as f64)),
                    M::ViewByteOffset => Ok(Value::Number(start as f64)),
                    M::ViewGet(kind) | M::ViewSet(kind) => {
                        let offset = self.index_argument(first, "offset")?;
                        let size = kind.size();
                        if start + offset + size > end {
                            return Err(
                                self.range_error("Offset is outside the bounds of the DataView")
                            );
                        }
                        let little = argument(
                            &arguments,
                            if matches!(method, M::ViewGet(_)) {
                                1
                            } else {
                                2
                            },
                        )
                        .truthy();
                        let position = start + offset;
                        if let M::ViewGet(_) = method {
                            let mut bytes = buffer.borrow()[position..position + size].to_vec();
                            if !little {
                                bytes.reverse();
                            }
                            Ok(Value::Number(kind.read(&bytes)))
                        } else {
                            let number = self.coerce_number(argument(&arguments, 1))?;
                            let mut bytes = kind.encode(number);
                            if !little {
                                bytes.reverse();
                            }
                            buffer.borrow_mut()[position..position + size].copy_from_slice(&bytes);
                            Ok(Value::Undefined)
                        }
                    }
                    _ => unreachable!("matched above"),
                }
            }
        }
    }

    fn numeric_comparator(&self) -> Value {
        Value::Builtin(Builtin::Native(Native::NumericCompare))
    }

    fn kind_of_constructor(&mut self, constructor: &Value) -> Result<ElementKind, ScriptError> {
        if let Value::Object(object) = constructor
            && let ObjectClass::Native(Native::Typed(TypedMethod::Constructor(kind))) =
                object.borrow().class
        {
            return Ok(kind);
        }
        Err(self.type_error("this is not a typed array constructor"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_convert_like_the_spec() {
        assert_eq!(ElementKind::Int8.encode(200.0), vec![200u8]);
        assert_eq!(ElementKind::Int8.read(&[200]), -56.0);
        assert_eq!(ElementKind::Uint8.encode(-1.0), vec![255]);
        assert_eq!(ElementKind::Uint8Clamped.encode(300.0), vec![255]);
        assert_eq!(ElementKind::Uint8Clamped.encode(2.5), vec![2]);
        assert_eq!(ElementKind::Uint8Clamped.encode(3.5), vec![4]);
        assert_eq!(
            ElementKind::Uint16.read(&ElementKind::Uint16.encode(70000.0)),
            4464.0
        );
        assert_eq!(
            ElementKind::Int32.read(&ElementKind::Int32.encode(-1.0)),
            -1.0
        );
        assert_eq!(
            ElementKind::Float32.read(&ElementKind::Float32.encode(0.1)),
            f64::from(0.1f32)
        );
        assert!(
            ElementKind::Float64
                .read(&ElementKind::Float64.encode(f64::NAN))
                .is_nan()
        );
    }
}
