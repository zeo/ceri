// abstract operations from the language specification: type conversion, the operators,
// property access across ordinary objects and host values, globals and the iteration protocol

use super::ast::BinaryOperator;
use super::object::{
    JsObject, ObjectClass, ObjectHandle, PRIVATE_PREFIX, Property, Slot, array_index, new_object,
};
use super::vm::{DelegateResult, ResumeMode};
use super::*;

// how the vm names a value it starts iterating, as v8's call printer finds it
pub(super) enum Callsite {
    // the whole not-iterable message, from the iterated source
    Source(JsStr),
    // the source of a spread call argument
    Argument(JsStr),
}

// the iteration state behind for-of, spread and destructuring; host collections iterate a
// snapshot, arrays read live by index, and other objects go through next()
pub(super) enum VmIterator {
    Array {
        array: ArrayHandle,
        index: usize,
    },
    List {
        values: Vec<Value>,
        index: usize,
    },
    Keys {
        keys: Vec<String>,
        index: usize,
        object: Value,
    },
    Object {
        iterator: Value,
        next: Value,
    },
    AsyncFromSync(Box<VmIterator>),
    Done,
}

impl VmIterator {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        match self {
            Self::Array { array, .. } => tracer.mark_array(array.clone()),
            Self::List { values, .. } => {
                for value in values {
                    tracer.trace_value(value);
                }
            }
            Self::Keys { object, .. } => tracer.trace_value(object),
            Self::Object { iterator, next } => {
                tracer.trace_value(iterator);
                tracer.trace_value(next);
            }
            Self::AsyncFromSync(inner) => inner.trace(tracer),
            Self::Done => {}
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Hint {
    Default,
    Number,
    String,
}

// a plain object holding the given data properties in order, inheriting from Object.prototype
pub(super) fn plain_object(entries: impl IntoIterator<Item = (String, Value)>) -> Value {
    Value::Object(new_object(entries.into_iter().collect()))
}

pub(super) fn is_primitive(value: &Value) -> bool {
    matches!(
        value,
        Value::Undefined
            | Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Symbol(_)
            | Value::BigInt(_)
            | Value::Uninitialized
    )
}

pub(super) fn is_callable(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.borrow().is_callable(),
        Value::Binding(cell) | Value::ConstBinding(cell) => is_callable(&cell.borrow()),
        Value::Builtin(_)
        | Value::ResponseConstructor
        | Value::RequestConstructor
        | Value::HeadersConstructor
        | Value::FormDataConstructor
        | Value::BlobConstructor
        | Value::FileConstructor
        | Value::AbortControllerConstructor
        | Value::WorkerConstructor => true,
        _ => false,
    }
}

const JS_WHITESPACE: fn(char) -> bool =
    |character| character.is_whitespace() || character == '\u{feff}';

// Number::toString(10): the shortest digits that round-trip, laid out as the spec describes
pub(super) fn number_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value == 0.0 {
        return "0".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if value < 0.0 {
        return format!("-{}", number_to_string(-value));
    }
    let formatted = format!("{value:e}");
    let (mantissa, exponent) = formatted.split_once('e').expect("exponential form");
    let exponent: i32 = exponent.parse().expect("exponent digits");
    let digits: String = mantissa
        .chars()
        .filter(|character| *character != '.')
        .collect();
    let k = digits.len() as i32;
    let n = exponent + 1;
    if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let sign = if n - 1 < 0 { '-' } else { '+' };
        let power = (n - 1).abs();
        if k == 1 {
            format!("{digits}e{sign}{power}")
        } else {
            format!("{}.{}e{sign}{power}", &digits[..1], &digits[1..])
        }
    }
}

// StringToNumber: decimal literals, Infinity, and 0x, 0o and 0b integers, after trimming
pub(super) fn string_to_number(text: &str) -> f64 {
    let text = text.trim_matches(JS_WHITESPACE);
    if text.is_empty() {
        return 0.0;
    }
    match text {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    let radix = match text.get(..2) {
        Some("0x" | "0X") => Some(16),
        Some("0o" | "0O") => Some(8),
        Some("0b" | "0B") => Some(2),
        _ => None,
    };
    if let Some(radix) = radix {
        let digits = &text[2..];
        if digits.is_empty() {
            return f64::NAN;
        }
        let mut value = 0.0;
        for character in digits.chars() {
            let Some(digit) = character.to_digit(radix) else {
                return f64::NAN;
            };
            value = value * f64::from(radix) + f64::from(digit);
        }
        return value;
    }
    let decimal = text
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'E' | b'+' | b'-'));
    if !decimal {
        return f64::NAN;
    }
    text.parse::<f64>().unwrap_or(f64::NAN)
}

// what an operator makes of two numbers, for the ones that need nothing else
#[inline(always)]
pub(super) fn number_binary(operator: BinaryOperator, a: f64, b: f64) -> Option<Value> {
    use BinaryOperator as B;
    Some(match operator {
        B::Add => Value::Number(a + b),
        B::Subtract => Value::Number(a - b),
        B::Multiply => Value::Number(a * b),
        B::Divide => Value::Number(a / b),
        B::Less => Value::Bool(a < b),
        B::LessEqual => Value::Bool(a <= b),
        B::Greater => Value::Bool(a > b),
        B::GreaterEqual => Value::Bool(a >= b),
        B::StrictEqual | B::Equal => Value::Bool(a == b),
        B::StrictNotEqual | B::NotEqual => Value::Bool(a != b),
        B::BitOr => Value::Number(f64::from(to_int32_number(a) | to_int32_number(b))),
        B::BitAnd => Value::Number(f64::from(to_int32_number(a) & to_int32_number(b))),
        B::BitXor => Value::Number(f64::from(to_int32_number(a) ^ to_int32_number(b))),
        _ => return None,
    })
}

#[inline]
pub(super) fn to_int32_number(value: f64) -> i32 {
    // a number that is already a 32-bit integer, as nearly all are, converts as it stands
    let whole = value as i32;
    if f64::from(whole) == value {
        return whole;
    }
    to_uint32_number(value) as i32
}

pub(super) fn to_uint32_number(value: f64) -> u32 {
    if !value.is_finite() {
        return 0;
    }
    value.trunc().rem_euclid(4_294_967_296.0) as u32
}

// the description a symbol was created with; well-known symbols read as Symbol.name
pub(super) fn symbol_description(key: &str) -> Option<String> {
    if let Some(name) = key.strip_prefix("@@") {
        return Some(format!("Symbol.{name}"));
    }
    let rest = key.strip_prefix(super::object::SYMBOL_PREFIX)?;
    rest.split_once(':')
        .map(|(_, description)| description.to_string())
}

fn display_key(key: &str) -> String {
    if super::object::is_symbol_key(key) {
        format!("Symbol({})", symbol_description(key).unwrap_or_default())
    } else if let Some(rest) = key.strip_prefix(PRIVATE_PREFIX) {
        format!("#{}", rest.split_once(':').map_or(rest, |(_, name)| name))
    } else {
        key.to_string()
    }
}

// the string a typeof answers, made once per kind instead of once per typeof
pub(super) fn type_name(kind: &'static str) -> JsStr {
    thread_local! {
        static NAMES: RefCell<Vec<(&'static str, JsStr)>> = const { RefCell::new(Vec::new()) };
    }
    NAMES.with(|names| {
        let mut names = names.borrow_mut();
        if let Some((_, name)) = names.iter().find(|(known, _)| *known == kind) {
            return name.clone();
        }
        let name: JsStr = kind.into();
        names.push((kind, name.clone()));
        name
    })
}

pub(super) fn is_missing_property(error: &ScriptError) -> bool {
    error.message.contains(" has no property ")
}

impl ScriptRuntime {
    // ---- errors ----

    pub(super) fn make_error(&self, kind: &str, message: &str) -> Value {
        let mut object =
            JsObject::with_prototype(Some(self.error_prototype(kind)), ObjectClass::Error);
        object.define(
            "message".to_string(),
            Property::hidden(Value::String(message.to_string().into())),
        );
        self.with_stack(object)
    }

    // an error object with its stack: what it shows as its name and message, then the frames
    // running as it is made
    pub(super) fn with_stack(&self, object: JsObject) -> Value {
        let object = new_object(object);
        let mut stack = super::object_display(&object);
        self.write_stack(&mut stack);
        object.borrow_mut().define(
            "stack".to_string(),
            Property::hidden(Value::String(stack.into())),
        );
        Value::Object(object)
    }

    pub(super) fn error_prototype(&self, kind: &str) -> ObjectHandle {
        let intrinsics = &self.intrinsics;
        match kind {
            "TypeError" => intrinsics.type_error_prototype.clone(),
            "RangeError" => intrinsics.range_error_prototype.clone(),
            "SyntaxError" => intrinsics.syntax_error_prototype.clone(),
            "ReferenceError" => intrinsics.reference_error_prototype.clone(),
            "EvalError" => intrinsics.eval_error_prototype.clone(),
            "URIError" => intrinsics.uri_error_prototype.clone(),
            "AggregateError" => intrinsics.aggregate_error_prototype.clone(),
            "SuppressedError" => intrinsics.suppressed_error_prototype.clone(),
            _ => intrinsics.error_prototype.clone(),
        }
    }

    // the message carried by a thrown value, for the Rust-side error
    pub(super) fn thrown_message(&self, value: &Value) -> String {
        match value {
            Value::Object(object) => {
                let mut current = Some(object.clone());
                while let Some(object) = current {
                    let object = object.borrow();
                    if let Some(Value::String(message)) = object.get("message") {
                        return message.to_string();
                    }
                    current = object.prototype_or(&self.intrinsics);
                }
                "[object Object]".to_string()
            }
            Value::Error(error) => error.message.clone(),
            other => other.string_value(),
        }
    }

    // the name an error object carries through its prototype chain, empty for other values
    fn thrown_name(&self, value: &Value) -> String {
        let Value::Object(object) = value else {
            return String::new();
        };
        let mut current = Some(object.clone());
        while let Some(object) = current {
            let object = object.borrow();
            if let Some(Value::String(name)) = object.get("name") {
                return name.to_string();
            }
            current = object.prototype_or(&self.intrinsics);
        }
        String::new()
    }

    pub(super) fn throw_value(&mut self, value: Value) -> ScriptError {
        let message = self.thrown_message(&value);
        let name = self.thrown_name(&value);
        self.pending_exception = Some((value, message.clone()));
        ScriptError {
            name,
            ..runtime_error(message)
        }
    }

    pub(super) fn throw_error(&mut self, kind: &str, message: impl Into<String>) -> ScriptError {
        let error = self.make_error(kind, &message.into());
        self.throw_value(error)
    }

    pub(super) fn type_error(&mut self, message: impl Into<String>) -> ScriptError {
        self.throw_error("TypeError", message)
    }

    pub(super) fn range_error(&mut self, message: impl Into<String>) -> ScriptError {
        self.throw_error("RangeError", message)
    }

    pub(super) fn reference_error(&mut self, message: impl Into<String>) -> ScriptError {
        self.throw_error("ReferenceError", message)
    }

    // the JavaScript value behind a Rust error: the thrown value, or, for a host API that
    // failed without one, the TypeError a browser raises for misuse
    pub(super) fn take_thrown(&mut self, error: ScriptError) -> Value {
        match self.pending_exception.take() {
            Some((value, message)) if message == error.message => value,
            _ => {
                let name = error.name.clone();
                self.make_error(&name, &error.message)
            }
        }
    }

    // ---- conversions ----

    pub(super) fn coerce_primitive(
        &mut self,
        value: Value,
        hint: Hint,
    ) -> Result<Value, ScriptError> {
        match &value {
            Value::Object(_)
            | Value::Element(_)
            | Value::Document
            | Value::Event(_)
            | Value::Range(_)
            | Value::Selection(_)
            | Value::Promise(_) => {
                let exotic = self.get_property(&value, "@@toPrimitive")?;
                if !matches!(exotic, Value::Undefined | Value::Null) {
                    let hint_name = match hint {
                        Hint::Default => "default",
                        Hint::Number => "number",
                        Hint::String => "string",
                    };
                    let result = self.call_function(
                        exotic,
                        value.clone(),
                        vec![Value::String(hint_name.to_string().into())],
                    )?;
                    if !is_primitive(&result) {
                        return Err(self.type_error("Cannot convert object to primitive value"));
                    }
                    return Ok(result);
                }
                let order = if hint == Hint::String {
                    ["toString", "valueOf"]
                } else {
                    ["valueOf", "toString"]
                };
                for name in order {
                    let method = self.get_property(&value, name)?;
                    if is_callable(&method) {
                        let result = self.call_function(method, value.clone(), Vec::new())?;
                        if is_primitive(&result) {
                            return Ok(result);
                        }
                    }
                }
                Err(self.type_error("Cannot convert object to primitive value"))
            }
            Value::Binding(cell) | Value::ConstBinding(cell) => {
                let inner = cell.borrow().clone();
                self.coerce_primitive(inner, hint)
            }
            Value::Array(array) => {
                let array = array.clone();
                Ok(Value::String(self.join_array(&array, ",")?.into()))
            }
            Value::Error(error) => Ok(Value::String(if error.message.is_empty() {
                error.name.as_str().into()
            } else {
                format!("{}: {}", error.name, error.message).into()
            })),
            value if is_primitive(value) => Ok(value.clone()),
            other => Ok(Value::String(other.string_value().into())),
        }
    }

    pub(super) fn join_array(
        &mut self,
        array: &ArrayHandle,
        separator: &str,
    ) -> Result<String, ScriptError> {
        if self
            .joining
            .iter()
            .any(|joining| ArrayHandle::ptr_eq(joining, array))
        {
            return Ok(String::new());
        }
        self.joining.push(array.clone());
        let values = array.borrow().clone();
        let mut parts = Vec::with_capacity(values.len());
        let mut outcome = Ok(());
        for value in values {
            match value {
                Value::Undefined | Value::Null | Value::Uninitialized => parts.push(String::new()),
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
        outcome.map(|()| parts.join(separator))
    }

    pub(super) fn coerce_number(&mut self, value: Value) -> Result<f64, ScriptError> {
        match value {
            Value::Number(number) => Ok(number),
            Value::Undefined => Ok(f64::NAN),
            Value::Null => Ok(0.0),
            Value::Bool(value) => Ok(if value { 1.0 } else { 0.0 }),
            Value::String(text) => Ok(string_to_number(&text)),
            Value::Symbol(_) => Err(self.type_error("Cannot convert a Symbol value to a number")),
            Value::BigInt(_) => Err(self.type_error("Cannot convert a BigInt value to a number")),
            other => {
                let primitive = self.coerce_primitive(other, Hint::Number)?;
                self.coerce_number(primitive)
            }
        }
    }

    // ToNumeric: a Number or a BigInt
    pub(super) fn coerce_numeric(&mut self, value: Value) -> Result<Value, ScriptError> {
        match value {
            Value::Number(_) | Value::BigInt(_) => Ok(value),
            other => match self.coerce_primitive(other, Hint::Number)? {
                Value::BigInt(value) => Ok(Value::BigInt(value)),
                primitive => Ok(Value::Number(self.coerce_number(primitive)?)),
            },
        }
    }

    pub(super) fn negate_numeric(&mut self, value: Value) -> Result<Value, ScriptError> {
        Ok(match self.coerce_numeric(value)? {
            Value::BigInt(value) => Value::BigInt(Rc::new(value.negate())),
            numeric => Value::Number(-self.coerce_number(numeric)?),
        })
    }

    // ToBigInt
    pub(super) fn coerce_bigint(
        &mut self,
        value: Value,
    ) -> Result<Rc<super::bigint::BigInt>, ScriptError> {
        match value {
            Value::BigInt(value) => Ok(value),
            Value::Bool(flag) => Ok(Rc::new(super::bigint::BigInt::from_i64(i64::from(flag)))),
            Value::String(text) => {
                match super::bigint::BigInt::parse(&text) {
                    Some(value) => Ok(Rc::new(value)),
                    None => Err(self
                        .throw_error("SyntaxError", format!("Cannot convert {text} to a BigInt"))),
                }
            }
            Value::Number(number) => Err(self.type_error(format!(
                "Cannot convert {} to a BigInt",
                number_to_string(number)
            ))),
            Value::Undefined | Value::Null | Value::Symbol(_) | Value::Uninitialized => Err(self
                .type_error(format!(
                    "Cannot convert {} to a BigInt",
                    value.string_value()
                ))),
            other => {
                let primitive = self.coerce_primitive(other, Hint::Number)?;
                self.coerce_bigint(primitive)
            }
        }
    }

    fn bigint_binary(
        &mut self,
        operator: BinaryOperator,
        left: &super::bigint::BigInt,
        right: &super::bigint::BigInt,
    ) -> Result<Value, ScriptError> {
        use BinaryOperator as B;
        let result = match operator {
            B::Add => left.add(right),
            B::Subtract => left.sub(right),
            B::Multiply => left.mul(right),
            B::Divide | B::Remainder => match left.divmod(right) {
                Some((quotient, remainder)) => {
                    if matches!(operator, B::Divide) {
                        quotient
                    } else {
                        remainder
                    }
                }
                None => return Err(self.range_error("Division by zero")),
            },
            B::Exponent => match left.pow(right) {
                Some(value) => value,
                None if right.is_negative() => {
                    return Err(self.range_error("Exponent must be non-negative"));
                }
                None => return Err(self.range_error("Maximum BigInt size exceeded")),
            },
            B::BitAnd => left.bitwise(right, |a, b| a & b),
            B::BitOr => left.bitwise(right, |a, b| a | b),
            B::BitXor => left.bitwise(right, |a, b| a ^ b),
            B::ShiftLeft | B::ShiftRight => {
                let count: i64 =
                    right
                        .to_string_radix(10)
                        .parse()
                        .unwrap_or(if right.is_negative() {
                            i64::MIN
                        } else {
                            i64::MAX
                        });
                let count = if matches!(operator, B::ShiftLeft) {
                    count
                } else {
                    count.saturating_neg()
                };
                match left.shift(count) {
                    Some(value) => value,
                    None => return Err(self.range_error("Maximum BigInt size exceeded")),
                }
            }
            B::ShiftRightUnsigned => {
                return Err(self.type_error("BigInts have no unsigned right shift, use >> instead"));
            }
            _ => unreachable!("only arithmetic reaches here"),
        };
        Ok(Value::BigInt(Rc::new(result)))
    }

    // the numeric operators once both sides are numeric: BigInts only combine with BigInts
    fn numeric_binary(
        &mut self,
        operator: BinaryOperator,
        left: Value,
        right: Value,
    ) -> Result<Value, ScriptError> {
        use BinaryOperator as B;
        let left = self.coerce_numeric(left)?;
        let right = self.coerce_numeric(right)?;
        match (&left, &right) {
            (Value::BigInt(a), Value::BigInt(b)) => {
                let (a, b) = (a.clone(), b.clone());
                self.bigint_binary(operator, &a, &b)
            }
            (Value::BigInt(_), _) | (_, Value::BigInt(_)) => {
                Err(self.type_error("Cannot mix BigInt and other types, use explicit conversions"))
            }
            _ => {
                let a = self.coerce_number(left)?;
                let b = self.coerce_number(right)?;
                Ok(Value::Number(match operator {
                    B::Add => a + b,
                    B::Subtract => a - b,
                    B::Multiply => a * b,
                    B::Divide => a / b,
                    B::Remainder => a % b,
                    B::Exponent => power(a, b),
                    B::ShiftLeft => {
                        f64::from(to_int32_number(a).wrapping_shl(to_uint32_number(b) & 31))
                    }
                    B::ShiftRight => f64::from(to_int32_number(a) >> (to_uint32_number(b) & 31)),
                    B::ShiftRightUnsigned => {
                        f64::from(to_uint32_number(a) >> (to_uint32_number(b) & 31))
                    }
                    B::BitAnd => f64::from(to_int32_number(a) & to_int32_number(b)),
                    B::BitOr => f64::from(to_int32_number(a) | to_int32_number(b)),
                    B::BitXor => f64::from(to_int32_number(a) ^ to_int32_number(b)),
                    _ => unreachable!("only arithmetic reaches here"),
                }))
            }
        }
    }

    pub(super) fn coerce_string(&mut self, value: Value) -> Result<String, ScriptError> {
        match value {
            Value::String(text) => Ok(text.to_string()),
            Value::Number(number) => Ok(number_to_string(number)),
            Value::Undefined | Value::Uninitialized => Ok("undefined".to_string()),
            Value::Null => Ok("null".to_string()),
            Value::Bool(value) => Ok(value.to_string()),
            Value::Symbol(_) => Err(self.type_error("Cannot convert a Symbol value to a string")),
            Value::BigInt(value) => Ok(value.to_string_radix(10)),
            Value::ClassList(handle) => {
                Ok(self.element_attribute(&handle, "class").unwrap_or_default())
            }
            other => {
                let primitive = self.coerce_primitive(other, Hint::String)?;
                self.coerce_string(primitive)
            }
        }
    }

    pub(super) fn coerce_key(&mut self, value: Value) -> Result<String, ScriptError> {
        match value {
            Value::String(text) => Ok(text.to_string()),
            Value::Symbol(key) => Ok(key.to_string()),
            Value::Number(number) => Ok(number_to_string(number)),
            other => {
                let primitive = self.coerce_primitive(other, Hint::String)?;
                match primitive {
                    Value::Symbol(key) => Ok(key.to_string()),
                    other => self.coerce_string(other),
                }
            }
        }
    }

    pub(super) fn coerce_int32(&mut self, value: Value) -> Result<i32, ScriptError> {
        Ok(to_int32_number(self.coerce_number(value)?))
    }

    pub(super) fn coerce_uint32(&mut self, value: Value) -> Result<u32, ScriptError> {
        Ok(to_uint32_number(self.coerce_number(value)?))
    }

    // wraps primitives in their object form; host values are already objects
    pub(super) fn coerce_object(&mut self, value: Value) -> Result<Value, ScriptError> {
        let (prototype, class) = match &value {
            Value::Undefined | Value::Null => {
                return Err(self.type_error("Cannot convert undefined or null to object"));
            }
            Value::Bool(value) => (
                self.intrinsics.boolean_prototype.clone(),
                ObjectClass::Boolean(*value),
            ),
            Value::Number(value) => (
                self.intrinsics.number_prototype.clone(),
                ObjectClass::Number(*value),
            ),
            Value::String(value) => (
                self.intrinsics.string_prototype.clone(),
                ObjectClass::String(value.clone()),
            ),
            Value::Symbol(value) => (
                self.intrinsics.symbol_prototype.clone(),
                ObjectClass::Symbol(value.clone()),
            ),
            Value::BigInt(value) => (
                self.intrinsics.bigint_prototype.clone(),
                ObjectClass::BigInt(value.clone()),
            ),
            _ => return Ok(value),
        };
        Ok(Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            class,
        ))))
    }

    pub(super) fn type_of(&self, value: &Value) -> &'static str {
        value_typeof(value)
    }

    // ---- operators ----

    pub(super) fn binary(
        &mut self,
        operator: BinaryOperator,
        left: Value,
        right: Value,
    ) -> Result<Value, ScriptError> {
        use BinaryOperator as B;
        if let (Value::Number(a), Value::Number(b)) = (&left, &right)
            && let Some(value) = number_binary(operator, *a, *b)
        {
            // two numbers own nothing, so there is nothing to drop
            std::mem::forget(left);
            std::mem::forget(right);
            return Ok(value);
        }
        Ok(match operator {
            B::Add => {
                let left = self.coerce_primitive(left, Hint::Default)?;
                let right = self.coerce_primitive(right, Hint::Default)?;
                if matches!(left, Value::String(_)) || matches!(right, Value::String(_)) {
                    let mut text = self.coerce_string(left)?;
                    text.push_str(&self.coerce_string(right)?);
                    Value::String(text.into())
                } else {
                    self.numeric_binary(operator, left, right)?
                }
            }
            B::Subtract
            | B::Multiply
            | B::Divide
            | B::Remainder
            | B::Exponent
            | B::ShiftLeft
            | B::ShiftRight
            | B::ShiftRightUnsigned
            | B::BitAnd
            | B::BitOr
            | B::BitXor => self.numeric_binary(operator, left, right)?,
            B::StrictEqual => Value::Bool(strict_equals(&left, &right)),
            B::StrictNotEqual => Value::Bool(!strict_equals(&left, &right)),
            B::Equal => Value::Bool(self.loose_equals(left, right)?),
            B::NotEqual => Value::Bool(!self.loose_equals(left, right)?),
            B::Less => Value::Bool(self.less_than(left, right, true)? == Some(true)),
            B::Greater => Value::Bool(self.less_than(right, left, false)? == Some(true)),
            B::LessEqual => Value::Bool(self.less_than(right, left, false)? == Some(false)),
            B::GreaterEqual => Value::Bool(self.less_than(left, right, true)? == Some(false)),
            B::InstanceOf => Value::Bool(self.instance_of(&left, &right)?),
            B::In => {
                if is_primitive(&right) {
                    return Err(self.type_error(format!(
                        "Cannot use 'in' operator to search for '{}' in {}",
                        left.string_value(),
                        right.string_value()
                    )));
                }
                let key = self.coerce_key(left)?;
                Value::Bool(self.has_property(&right, &key)?)
            }
        })
    }

    // IsLessThan; None stands for undefined, when either side is NaN
    fn less_than(
        &mut self,
        left: Value,
        right: Value,
        left_first: bool,
    ) -> Result<Option<bool>, ScriptError> {
        let (left, right) = if left_first {
            let left = self.coerce_primitive(left, Hint::Number)?;
            (left, self.coerce_primitive(right, Hint::Number)?)
        } else {
            let right = self.coerce_primitive(right, Hint::Number)?;
            (self.coerce_primitive(left, Hint::Number)?, right)
        };
        if let (Value::String(left), Value::String(right)) = (&left, &right) {
            return Ok(Some(left.encode_utf16().lt(right.encode_utf16())));
        }
        let bigint_of = |value: &Value| -> Option<Option<super::bigint::BigInt>> {
            match value {
                Value::BigInt(value) => Some(Some((**value).clone())),
                Value::String(text) => Some(super::bigint::BigInt::parse(text)),
                _ => None,
            }
        };
        match (&left, &right) {
            (Value::BigInt(_), Value::String(_))
            | (Value::String(_), Value::BigInt(_))
            | (Value::BigInt(_), Value::BigInt(_)) => {
                return Ok(
                    match (bigint_of(&left).flatten(), bigint_of(&right).flatten()) {
                        (Some(a), Some(b)) => Some(a.compare(&b) == std::cmp::Ordering::Less),
                        _ => None,
                    },
                );
            }
            (Value::BigInt(a), _) => {
                let b = self.coerce_number(right.clone())?;
                return Ok(a
                    .compare_f64(b)
                    .map(|order| order == std::cmp::Ordering::Less));
            }
            (_, Value::BigInt(b)) => {
                let a = self.coerce_number(left.clone())?;
                return Ok(b
                    .compare_f64(a)
                    .map(|order| order == std::cmp::Ordering::Greater));
            }
            _ => {}
        }
        let left = self.coerce_number(left)?;
        let right = self.coerce_number(right)?;
        if left.is_nan() || right.is_nan() {
            return Ok(None);
        }
        Ok(Some(left < right))
    }

    pub(super) fn loose_equals(&mut self, left: Value, right: Value) -> Result<bool, ScriptError> {
        let left = left.resolve_owned();
        let right = right.resolve_owned();
        Ok(match (&left, &right) {
            (Value::Undefined | Value::Null, Value::Undefined | Value::Null) => true,
            (Value::Undefined | Value::Null, _) | (_, Value::Undefined | Value::Null) => false,
            (Value::BigInt(a), Value::Number(b)) | (Value::Number(b), Value::BigInt(a)) => {
                a.compare_f64(*b) == Some(std::cmp::Ordering::Equal)
            }
            (Value::BigInt(a), Value::String(text)) | (Value::String(text), Value::BigInt(a)) => {
                super::bigint::BigInt::parse(text).is_some_and(|b| **a == b)
            }
            (Value::Number(_), Value::String(text)) => {
                strict_equals(&left, &Value::Number(string_to_number(text)))
            }
            (Value::String(text), Value::Number(_)) => {
                strict_equals(&Value::Number(string_to_number(text)), &right)
            }
            (Value::Bool(value), _) => {
                return self.loose_equals(Value::Number(if *value { 1.0 } else { 0.0 }), right);
            }
            (_, Value::Bool(value)) => {
                return self.loose_equals(left, Value::Number(if *value { 1.0 } else { 0.0 }));
            }
            (Value::Number(_) | Value::String(_) | Value::Symbol(_), other)
                if !is_primitive(other) =>
            {
                let primitive = self.coerce_primitive(right, Hint::Default)?;
                return self.loose_equals(left, primitive);
            }
            (other, Value::Number(_) | Value::String(_) | Value::Symbol(_))
                if !is_primitive(other) =>
            {
                let primitive = self.coerce_primitive(left, Hint::Default)?;
                return self.loose_equals(primitive, right);
            }
            _ => strict_equals(&left, &right),
        })
    }

    pub(super) fn instance_of(
        &mut self,
        value: &Value,
        constructor: &Value,
    ) -> Result<bool, ScriptError> {
        if is_primitive(constructor) {
            return Err(self.type_error("Right-hand side of 'instanceof' is not an object"));
        }
        if matches!(constructor, Value::Object(_)) {
            let checker = self.get_property(constructor, "@@hasInstance")?;
            if !matches!(checker, Value::Undefined | Value::Null)
                && !self.is_default_has_instance(&checker)
            {
                let result =
                    self.call_function(checker, constructor.clone(), vec![value.clone()])?;
                return Ok(result.truthy());
            }
        }
        self.ordinary_has_instance(constructor, value)
    }

    fn is_default_has_instance(&self, checker: &Value) -> bool {
        matches!(checker, Value::Object(object) if matches!(object.borrow().class, ObjectClass::Native(ref native) if matches!(**native, Native::FunctionHasInstance)))
    }

    pub(super) fn ordinary_has_instance(
        &mut self,
        constructor: &Value,
        value: &Value,
    ) -> Result<bool, ScriptError> {
        if !is_callable(constructor) {
            return Err(self.type_error("Right-hand side of 'instanceof' is not callable"));
        }
        if let Value::Object(object) = constructor
            && let ObjectClass::Bound(bound) = &object.borrow().class
        {
            let target = bound.target.clone();
            return self.instance_of(value, &target);
        }
        if !matches!(constructor, Value::Object(_)) {
            return Ok(host_instance_of(value, constructor));
        }
        if is_primitive(value) {
            return Ok(false);
        }
        let Value::Object(prototype) = self.get_property(constructor, "prototype")? else {
            return Err(self.type_error("Function has non-object prototype in instanceof check"));
        };
        let mut current = self.prototype_of_value(value)?;
        while let Some(object) = current {
            if ObjectHandle::ptr_eq(&object, &prototype) {
                return Ok(true);
            }
            current = self.prototype_of_value(&Value::Object(object))?;
        }
        Ok(false)
    }

    // [[GetPrototypeOf]], consulting a proxy's trap
    pub(super) fn prototype_of_value(
        &mut self,
        value: &Value,
    ) -> Result<Option<ObjectHandle>, ScriptError> {
        if let Value::Object(object) = value
            && let Some(proxy) = super::reflect::proxy_of(object)
        {
            return self.proxy_prototype(&proxy);
        }
        Ok(self.prototype_of(value))
    }

    // ---- prototypes and properties ----

    // the [[Prototype]] of any value, with host values linked to the matching intrinsic
    pub(super) fn prototype_of(&self, value: &Value) -> Option<ObjectHandle> {
        let intrinsics = &self.intrinsics;
        Some(match value {
            Value::Undefined | Value::Null | Value::Uninitialized => return None,
            Value::Object(object) => {
                return object.borrow().prototype_or(&self.intrinsics);
            }
            Value::Element(_)
            | Value::Document
            | Value::Event(_)
            | Value::Range(_)
            | Value::Selection(_)
            | Value::Promise(_) => {
                let object = self
                    .wrapper_object(value)
                    .expect("nodes and events have wrappers");
                return object.borrow().prototype_or(&self.intrinsics);
            }
            Value::Window => intrinsics.dom.prototype("EventTarget"),
            Value::Binding(cell) | Value::ConstBinding(cell) => {
                return self.prototype_of(&cell.borrow());
            }
            Value::Bool(_) => intrinsics.boolean_prototype.clone(),
            Value::Number(_) => intrinsics.number_prototype.clone(),
            Value::String(_) => intrinsics.string_prototype.clone(),
            Value::Symbol(_) => intrinsics.symbol_prototype.clone(),
            Value::BigInt(_) => intrinsics.bigint_prototype.clone(),
            Value::Array(array) => match array.borrow().prototype() {
                None => intrinsics.array_prototype.clone(),
                Some(prototype) => return prototype.clone(),
            },
            Value::Error(error) if error.class_name == "DOMException" => {
                intrinsics.dom_exception_prototype.clone()
            }
            Value::Error(error) => self.error_prototype(&error.name),
            Value::Iterator(_) => intrinsics.iterator_prototype.clone(),
            Value::TypedArray(array) if array.borrow().object.is_some() => {
                return super::typed_object(array)
                    .borrow()
                    .prototype_or(&self.intrinsics);
            }
            Value::TypedArray(array) => {
                let kind = array.borrow().kind;
                let index = super::typed::ELEMENT_KINDS
                    .iter()
                    .position(|(candidate, _)| *candidate == kind)
                    .unwrap_or(0);
                intrinsics.typed_prototypes[index].clone()
            }
            Value::Bytes(_) => intrinsics.array_buffer_prototype.clone(),
            Value::Navigator => intrinsics.hosts["Navigator"].clone(),
            Value::History(_) => intrinsics.hosts["History"].clone(),
            Value::FileList(_) => intrinsics.hosts["FileList"].clone(),
            Value::CanvasContext(_) => intrinsics.hosts["CanvasRenderingContext2D"].clone(),
            Value::IndexedDbFactory(_) => intrinsics.hosts["IDBFactory"].clone(),
            value if is_callable(value) => intrinsics.function_prototype.clone(),
            _ => intrinsics.object_prototype.clone(),
        })
    }

    pub(super) fn get_property(&mut self, value: &Value, key: &str) -> Result<Value, ScriptError> {
        match value {
            Value::Object(object) => self.ordinary_get(object.clone(), key, value),
            Value::Element(_)
            | Value::Document
            | Value::Event(_)
            | Value::Range(_)
            | Value::Selection(_)
            | Value::Promise(_) => {
                let object = self
                    .wrapper_object(value)
                    .expect("nodes and events have wrappers");
                self.ordinary_get(object, key, value)
            }
            Value::Undefined | Value::Null | Value::Uninitialized => Err(self.type_error(format!(
                "Cannot read properties of {} (reading '{}')",
                value.string_value(),
                display_key(key)
            ))),
            Value::Binding(cell) | Value::ConstBinding(cell) => {
                let inner = cell.borrow().clone();
                self.get_property(&inner, key)
            }
            Value::Window => self.window_get(key),
            Value::String(text) => {
                if key == "length" {
                    return Ok(Value::Number(super::strings::utf16_length(text) as f64));
                }
                if let Some(index) = array_index(key) {
                    return Ok(
                        super::strings::unit_at(text, index as usize).unwrap_or(Value::Undefined)
                    );
                }
                self.host_get(value, key)
            }
            Value::Bool(_) | Value::Number(_) | Value::Symbol(_) | Value::BigInt(_) => {
                if let Value::Symbol(symbol) = value
                    && key == "description"
                {
                    return Ok(symbol_description(symbol)
                        .map(|text| Value::String(text.into()))
                        .unwrap_or(Value::Undefined));
                }
                let prototype = self
                    .prototype_of(value)
                    .expect("primitives have prototypes");
                self.ordinary_get(prototype, key, value)
            }
            Value::TypedArray(array) => match self.typed_get(array, key) {
                Some(element) => Ok(element),
                None if super::typed::canonical_numeric_index(key).is_some() => {
                    Ok(Value::Undefined)
                }
                None => self.ordinary_get(super::typed_object(array), key, value),
            },
            _ => self.host_get(value, key),
        }
    }

    fn host_get(&mut self, value: &Value, key: &str) -> Result<Value, ScriptError> {
        if let Value::TypedArray(array) = value
            && let Some(element) = self.typed_get(array, key)
        {
            return Ok(element);
        }
        if let Value::Array(array) = value {
            if let Some(found) = self.array_property_get(array, key, value)? {
                return Ok(found);
            }
            // an array answers only its length and indices itself, so a method name goes
            // straight to the prototype chain instead of through a missing-property error
            if key != "length" && key.parse::<usize>().is_err() {
                return match self.prototype_of(value) {
                    Some(prototype) => self.ordinary_get(prototype, key, value),
                    None => Ok(Value::Undefined),
                };
            }
        }
        match self.member(value.clone(), key) {
            Ok(found) => Ok(found),
            Err(error) if is_missing_property(&error) => match self.prototype_of(value) {
                Some(prototype) => self.ordinary_get(prototype, key, value),
                None => Ok(Value::Undefined),
            },
            Err(error) => Err(error),
        }
    }

    fn window_get(&mut self, key: &str) -> Result<Value, ScriptError> {
        if let Some(value) = self.get_global_accessor(key) {
            return value;
        }
        if let Some(value) = self.global.borrow().values.get(key) {
            return Ok(value.resolve());
        }
        match self.member(Value::Window, key) {
            Ok(value) => Ok(value),
            Err(error) if is_missing_property(&error) => {
                let prototype = self.intrinsics.dom.prototype("EventTarget");
                self.ordinary_get(prototype, key, &Value::Window)
            }
            Err(error) => Err(error),
        }
    }
}

// result of a property read whose getter or proxy trap can run as a tail call on the VM frame stack
pub(super) enum GetOutcome {
    Value(Value),
    Call {
        callee: Value,
        this: Value,
        arguments: Vec<Value>,
    },
}

impl ScriptRuntime {
    pub(super) fn get_property_tail(
        &mut self,
        value: &Value,
        key: &str,
    ) -> Result<GetOutcome, ScriptError> {
        match value {
            Value::Object(object) => self.ordinary_get_tail(object.clone(), key, value),
            Value::Element(_)
            | Value::Event(_)
            | Value::Range(_)
            | Value::Selection(_)
            | Value::Promise(_) => {
                let object = self
                    .wrapper_object(value)
                    .expect("nodes and events have wrappers");
                self.ordinary_get_tail(object, key, value)
            }
            Value::Binding(cell) | Value::ConstBinding(cell) => {
                let inner = cell.borrow().clone();
                self.get_property_tail(&inner, key)
            }
            Value::Bool(_) | Value::Number(_) | Value::Symbol(_) | Value::BigInt(_) => {
                if let Value::Symbol(symbol) = value
                    && key == "description"
                {
                    return Ok(GetOutcome::Value(
                        symbol_description(symbol)
                            .map(|text| Value::String(text.into()))
                            .unwrap_or(Value::Undefined),
                    ));
                }
                let prototype = self
                    .prototype_of(value)
                    .expect("primitives have prototypes");
                self.ordinary_get_tail(prototype, key, value)
            }
            _ => self.get_property(value, key).map(GetOutcome::Value),
        }
    }

    pub(super) fn ordinary_get(
        &mut self,
        object: ObjectHandle,
        key: &str,
        receiver: &Value,
    ) -> Result<Value, ScriptError> {
        match self.ordinary_get_tail(object, key, receiver)? {
            GetOutcome::Value(value) => Ok(value),
            GetOutcome::Call {
                callee,
                this,
                arguments,
            } => self.call_function(callee, this, arguments),
        }
    }

    pub(super) fn ordinary_get_tail(
        &mut self,
        object: ObjectHandle,
        key: &str,
        receiver: &Value,
    ) -> Result<GetOutcome, ScriptError> {
        enum Found {
            Data(Value),
            Getter(Option<Value>),
            Next(Option<ObjectHandle>),
        }
        let mut current = object;
        loop {
            if let Some(proxy) = super::reflect::proxy_of(&current) {
                return self.proxy_get_tail(&proxy, key, receiver);
            }
            let found = {
                let borrowed = current.borrow();
                match borrowed.property(key) {
                    Some(property) => match property.slot {
                        Slot::Data(value) => Found::Data(value.resolve()),
                        Slot::Accessor(accessors) => Found::Getter(accessors.get.clone()),
                    },
                    None => match exotic_own(&borrowed, key) {
                        Some(value) => Found::Data(value),
                        None => Found::Next(borrowed.prototype_or(&self.intrinsics)),
                    },
                }
            };
            match found {
                Found::Data(value) => return Ok(GetOutcome::Value(value)),
                Found::Getter(Some(getter)) => {
                    return Ok(GetOutcome::Call {
                        callee: getter,
                        this: receiver.clone(),
                        arguments: Vec::new(),
                    });
                }
                Found::Getter(None) => return Ok(GetOutcome::Value(Value::Undefined)),
                Found::Next(Some(next)) => current = next,
                Found::Next(None) => return Ok(GetOutcome::Value(Value::Undefined)),
            }
        }
    }

    pub(super) fn set_property(
        &mut self,
        target: &Value,
        key: &str,
        value: Value,
        strict: bool,
    ) -> Result<(), ScriptError> {
        match target {
            Value::Object(object) => self.ordinary_set(object.clone(), key, value, target, strict),
            Value::Element(_)
            | Value::Document
            | Value::Event(_)
            | Value::Range(_)
            | Value::Selection(_)
            | Value::Promise(_) => {
                let object = self
                    .wrapper_object(target)
                    .expect("nodes and events have wrappers");
                self.ordinary_set(object, key, value, target, strict)
            }
            Value::Undefined | Value::Null | Value::Uninitialized => Err(self.type_error(format!(
                "Cannot set properties of {} (setting '{}')",
                target.string_value(),
                display_key(key)
            ))),
            Value::Binding(cell) | Value::ConstBinding(cell) => {
                let inner = cell.borrow().clone();
                self.set_property(&inner, key, value, strict)
            }
            Value::Window => self.window_set(key, value),
            Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Symbol(_)
            | Value::BigInt(_) => {
                if strict {
                    return Err(self.type_error(format!(
                        "Cannot create property '{}' on {} '{}'",
                        display_key(key),
                        value_typeof(target),
                        target.string_value()
                    )));
                }
                Ok(())
            }
            Value::TypedArray(array) => match array_index(key) {
                Some(index) => self.typed_set(array, index as usize, value),
                None if super::typed::canonical_numeric_index(key).is_some() => Ok(()),
                None => self.ordinary_set(super::typed_object(array), key, value, target, strict),
            },
            Value::Array(array) if key != "length" => {
                if self.array_property_set(array, key, value.clone(), strict)? {
                    return Ok(());
                }
                if let Some(index) = array_index(key) {
                    if let Some(refusal) = array_write_refusal(array, index as usize) {
                        return if strict {
                            Err(self.type_error(refusal))
                        } else {
                            Ok(())
                        };
                    }
                    return set_array_element(array, index as usize, value);
                }
                if array.borrow().integrity() != super::Integrity::Open {
                    return if strict {
                        Err(self.type_error(format!(
                            "Cannot add property {}, object is not extensible",
                            display_key(key)
                        )))
                    } else {
                        Ok(())
                    };
                }
                self.set_array_expando(array, key, value);
                Ok(())
            }
            _ => self.assign_member(target.clone(), key, value),
        }
    }

    pub(super) fn set_array_expando(&mut self, array: &ArrayHandle, key: &str, value: Value) {
        let mut array = array.borrow_mut();
        let properties = &mut array.extra_mut().properties;
        match properties.iter_mut().find(|(name, _)| name == key) {
            Some((_, property)) => property.slot = Slot::Data(value),
            None => properties.push((key.to_string(), Property::data(value))),
        }
    }

    // what an array's own property list holds for a key, read as a get would: a getter runs
    // with the array as this
    fn array_property_get(
        &mut self,
        array: &ArrayHandle,
        key: &str,
        receiver: &Value,
    ) -> Result<Option<Value>, ScriptError> {
        let slot = match array.borrow().own(key) {
            Some(property) => property.slot.clone(),
            None => return Ok(None),
        };
        Ok(Some(match slot {
            Slot::Data(value) => value,
            Slot::Accessor(accessors) => match accessors.get {
                Some(getter) => self.call_function(getter, receiver.clone(), Vec::new())?,
                None => Value::Undefined,
            },
        }))
    }

    // a write to an array through its own property list; false when the key is not there
    fn array_property_set(
        &mut self,
        array: &ArrayHandle,
        key: &str,
        value: Value,
        strict: bool,
    ) -> Result<bool, ScriptError> {
        let found = array
            .borrow()
            .own(key)
            .map(|property| (property.slot.clone(), property.writable));
        let Some((slot, writable)) = found else {
            return Ok(false);
        };
        match slot {
            Slot::Accessor(accessors) => match accessors.set {
                Some(setter) => {
                    self.call_function(setter, Value::Array(array.clone()), vec![value])?;
                }
                None if strict => {
                    return Err(self.type_error(format!(
                        "Cannot set property {} of [object Array] which has only a getter",
                        display_key(key)
                    )));
                }
                None => {}
            },
            Slot::Data(_)
                if !writable || array.borrow().integrity() == super::Integrity::Frozen =>
            {
                if strict {
                    return Err(self.type_error(format!(
                        "Cannot assign to read only property '{}' of object '[object Array]'",
                        display_key(key)
                    )));
                }
            }
            Slot::Data(_) => self.set_array_expando(array, key, value),
        }
        Ok(true)
    }

    fn window_set(&mut self, key: &str, value: Value) -> Result<(), ScriptError> {
        let host_property = key == "location"
            || (self.worker_sender.is_some() && matches!(key, "onmessage" | "onerror"));
        if host_property {
            return self.assign_member(Value::Window, key, value);
        }
        self.set_global(key, value, false)
    }

    pub(super) fn ordinary_set(
        &mut self,
        object: ObjectHandle,
        key: &str,
        value: Value,
        receiver: &Value,
        strict: bool,
    ) -> Result<(), ScriptError> {
        enum Found {
            Accessor(Option<Value>),
            ReadOnly,
            Writable,
            Next(Option<ObjectHandle>),
        }
        let mut current = object;
        loop {
            if let Some(proxy) = super::reflect::proxy_of(&current) {
                return self.proxy_set(&proxy, key, value, receiver, strict);
            }
            let found = {
                let borrowed = current.borrow();
                match borrowed.property(key) {
                    Some(property) => match property.slot {
                        Slot::Accessor(accessors) => Found::Accessor(accessors.set.clone()),
                        Slot::Data(_) if !property.writable => Found::ReadOnly,
                        Slot::Data(_) => Found::Writable,
                    },
                    None => {
                        if exotic_own(&borrowed, key).is_some() {
                            Found::ReadOnly
                        } else {
                            Found::Next(borrowed.prototype_or(&self.intrinsics))
                        }
                    }
                }
            };
            match found {
                Found::Accessor(setter) => {
                    return match setter {
                        Some(setter) => self
                            .call_function(setter, receiver.clone(), vec![value])
                            .map(|_| ()),
                        None if strict => Err(self.type_error(format!(
                            "Cannot set property {} of {} which has only a getter",
                            display_key(key),
                            receiver_label(receiver)
                        ))),
                        None => Ok(()),
                    };
                }
                Found::ReadOnly => {
                    return if strict {
                        Err(self.type_error(format!(
                            "Cannot assign to read only property '{}' of {}",
                            display_key(key),
                            receiver_label(receiver)
                        )))
                    } else {
                        Ok(())
                    };
                }
                Found::Writable => break,
                Found::Next(Some(next)) => current = next,
                Found::Next(None) => break,
            }
        }
        let receiver_object = match receiver {
            Value::Object(object) => object.clone(),
            Value::TypedArray(array) => super::typed_object(array),
            other => match self.wrapper_object(other) {
                Some(object) => object,
                None => return self.set_property(receiver, key, value, strict),
            },
        };
        let receiver_object = &receiver_object;
        // a proxy receiver takes the write through its defineProperty path
        if let Some(proxy) = super::reflect::proxy_of(receiver_object) {
            let existing = self.proxy_own_property(&proxy, key)?;
            let mut entries = vec![("value".to_string(), value)];
            if existing.is_none() {
                for attribute in ["writable", "enumerable", "configurable"] {
                    entries.push((attribute.to_string(), Value::Bool(true)));
                }
            }
            let descriptor = self.object_from_entries(entries);
            if !self.proxy_define(&proxy, key, &descriptor)? && strict {
                return Err(self.type_error(format!(
                    "'defineProperty' on proxy: trap returned falsish for property '{}'",
                    display_key(key)
                )));
            }
            return Ok(());
        }
        let mut receiver_object = receiver_object.borrow_mut();
        if let Some(slot) = receiver_object.slot_mut(key) {
            if let Slot::Data(existing) = slot {
                match existing {
                    Value::Binding(cell) => *cell.borrow_mut() = value,
                    _ => *existing = value,
                }
            }
            return Ok(());
        }
        if !receiver_object.extensible {
            drop(receiver_object);
            if strict {
                return Err(self.type_error(format!(
                    "Cannot add property {}, object is not extensible",
                    display_key(key)
                )));
            }
            return Ok(());
        }
        receiver_object.define(key.to_string(), Property::data(value));
        Ok(())
    }

    pub(super) fn get_index(&mut self, object: &Value, key: Value) -> Result<Value, ScriptError> {
        match self.get_index_tail(object, key)? {
            GetOutcome::Value(value) => Ok(value),
            GetOutcome::Call {
                callee,
                this,
                arguments,
            } => self.call_function(callee, this, arguments),
        }
    }

    pub(super) fn get_index_tail(
        &mut self,
        object: &Value,
        key: Value,
    ) -> Result<GetOutcome, ScriptError> {
        if let Value::Number(number) = key {
            match object {
                Value::Array(array) => {
                    if number >= 0.0 && number.fract() == 0.0 {
                        let index = number as usize;
                        let own = array.borrow().get(index).cloned();
                        if let Some(value) = own
                            && !matches!(value, Value::Uninitialized)
                        {
                            return Ok(GetOutcome::Value(value));
                        }
                        // an index the array does not have is read from its prototypes
                        if (index as f64) == number {
                            let key = index.to_string();
                            if let Some(found) = self.array_property_get(array, &key, object)? {
                                return Ok(GetOutcome::Value(found));
                            }
                            if !self.inherits(object, &key) {
                                return Ok(GetOutcome::Value(Value::Undefined));
                            }
                            let Some(prototype) = self.prototype_of(object) else {
                                return Ok(GetOutcome::Value(Value::Undefined));
                            };
                            return self.ordinary_get_tail(prototype, &key, object);
                        }
                    }
                }
                Value::Object(_) | Value::Window | Value::Undefined | Value::Null => {}
                Value::String(_)
                | Value::NodeList(_)
                | Value::FileList(_)
                | Value::IndexedDbNameList(_)
                | Value::Bytes(_) => {
                    if number >= 0.0 && number.fract() == 0.0 {
                        return self
                            .index(object.clone(), Value::Number(number))
                            .map(GetOutcome::Value);
                    }
                }
                Value::TypedArray(array) if number >= 0.0 && number.fract() == 0.0 => {
                    let value = super::typed::read_element(&array.borrow(), number as usize);
                    return Ok(GetOutcome::Value(value.unwrap_or(Value::Undefined)));
                }
                _ => {}
            }
        }
        let key = self.coerce_key(key)?;
        self.get_property_tail(object, &key)
    }

    pub(super) fn set_index(
        &mut self,
        object: &Value,
        key: Value,
        value: Value,
        strict: bool,
    ) -> Result<(), ScriptError> {
        if let Value::Number(number) = key
            && number >= 0.0
            && number.fract() == 0.0
        {
            match object {
                Value::Array(array) => {
                    let index = runtime_array_index(&Value::Number(number))?;
                    // an index only needs the named-property path when the array has some
                    if !array.borrow().properties().is_empty()
                        && self.array_property_set(
                            array,
                            &index.to_string(),
                            value.clone(),
                            strict,
                        )?
                    {
                        return Ok(());
                    }
                    if let Some(refusal) = array_write_refusal(array, index) {
                        return if strict {
                            Err(self.type_error(refusal))
                        } else {
                            Ok(())
                        };
                    }
                    return set_array_element(array, index, value);
                }
                Value::TypedArray(array) => return self.typed_set(array, number as usize, value),
                _ => {}
            }
        }
        let key = self.coerce_key(key)?;
        self.set_property(object, &key, value, strict)
    }

    pub(super) fn delete_property(
        &mut self,
        target: &Value,
        key: &str,
        strict: bool,
    ) -> Result<bool, ScriptError> {
        if let Some(object) = self.wrapper_object(target) {
            return self.delete_property(&Value::Object(object), key, strict);
        }
        if let Value::TypedArray(array) = target {
            if let Some(index) = array_index(key) {
                return Ok(index as usize >= super::typed::element_count(&array.borrow()));
            }
            let object = Value::Object(super::typed_object(array));
            return self.delete_property(&object, key, strict);
        }
        match target {
            Value::Object(object) if super::reflect::proxy_of(object).is_some() => {
                let proxy = super::reflect::proxy_of(object).expect("checked above");
                self.proxy_delete(&proxy, key, strict)
            }
            Value::Object(object) => {
                let configurable = object
                    .borrow()
                    .property(key)
                    .map(|property| property.configurable);
                match configurable {
                    None => Ok(true),
                    Some(true) => {
                        object.borrow_mut().remove(key);
                        Ok(true)
                    }
                    Some(false) if strict => Err(self.type_error(format!(
                        "Cannot delete property '{}' of {}",
                        display_key(key),
                        receiver_label(target)
                    ))),
                    Some(false) => Ok(false),
                }
            }
            Value::Undefined | Value::Null => Err(self.type_error(format!(
                "Cannot convert undefined or null to object (deleting '{}')",
                display_key(key)
            ))),
            Value::Array(_) if key == "length" => {
                if strict {
                    return Err(
                        self.type_error("Cannot delete property 'length' of [object Array]")
                    );
                }
                Ok(false)
            }
            Value::Array(array) => {
                let configurable = array
                    .borrow()
                    .own(key)
                    .map(|property| property.configurable);
                match configurable {
                    Some(true) => {
                        if let Some(extra) = array.borrow_mut().extra.as_mut() {
                            extra.properties.retain(|(name, _)| name != key);
                        }
                        return Ok(true);
                    }
                    Some(false) if strict => {
                        return Err(self.type_error(format!(
                            "Cannot delete property '{}' of [object Array]",
                            display_key(key)
                        )));
                    }
                    Some(false) => return Ok(false),
                    None => {}
                }
                if let Some(index) = array_index(key) {
                    let present = array
                        .borrow()
                        .get(index as usize)
                        .is_some_and(|value| !matches!(value, Value::Uninitialized));
                    if present && array.borrow().integrity() >= super::Integrity::Sealed {
                        return if strict {
                            Err(self.type_error(format!(
                                "Cannot delete property '{key}' of [object Array]"
                            )))
                        } else {
                            Ok(false)
                        };
                    }
                    let mut array = array.borrow_mut();
                    if let Some(slot) = array.get_mut(index as usize) {
                        *slot = Value::Uninitialized;
                    }
                }
                Ok(true)
            }
            Value::Window => {
                let mut global = self.global.borrow_mut();
                if global
                    .accessors
                    .get(key)
                    .is_some_and(|property| !property.configurable)
                {
                    return Ok(false);
                }
                global.accessors.remove(key);
                global.values.remove(key);
                global.hidden.remove(key);
                Ok(true)
            }
            _ => Ok(true),
        }
    }

    pub(super) fn has_property(&mut self, target: &Value, key: &str) -> Result<bool, ScriptError> {
        if let Some(object) = self.wrapper_object(target) {
            return self.has_property(&Value::Object(object), key);
        }
        match target {
            Value::Object(object) => {
                let mut current = Some(object.clone());
                while let Some(object) = current {
                    if let Some(proxy) = super::reflect::proxy_of(&object) {
                        return self.proxy_has(&proxy, key);
                    }
                    let borrowed = object.borrow();
                    if borrowed.contains_key(key) || exotic_own(&borrowed, key).is_some() {
                        return Ok(true);
                    }
                    current = borrowed.prototype_or(&self.intrinsics);
                }
                Ok(false)
            }
            Value::Array(array) => {
                if key == "length" || array.borrow().own(key).is_some() {
                    return Ok(true);
                }
                // an index nothing was put at is no property of the array, though one of its
                // prototypes may have it
                if let Some(index) = array_index(key) {
                    let own = array
                        .borrow()
                        .get(index as usize)
                        .is_some_and(|value| !matches!(value, Value::Uninitialized));
                    return Ok(own || self.inherits(target, key));
                }
                self.host_has(target, key)
            }
            Value::Window => {
                let own = {
                    let global = self.global.borrow();
                    global.values.contains_key(key) || global.accessors.contains_key(key)
                };
                if own {
                    return Ok(true);
                }
                self.host_has(target, key)
            }
            _ => self.host_has(target, key),
        }
    }

    // whether one of the value's prototypes has the property
    pub(super) fn inherits(&self, target: &Value, key: &str) -> bool {
        let mut current = self.prototype_of(target);
        while let Some(object) = current {
            let borrowed = object.borrow();
            if borrowed.contains_key(key) {
                return true;
            }
            current = borrowed.prototype_or(&self.intrinsics);
        }
        false
    }

    fn host_has(&mut self, target: &Value, key: &str) -> Result<bool, ScriptError> {
        match self.member(target.clone(), key) {
            Ok(_) => Ok(true),
            Err(error) if is_missing_property(&error) => {
                let mut current = self.prototype_of(target);
                while let Some(object) = current {
                    let borrowed = object.borrow();
                    if borrowed.contains_key(key) {
                        return Ok(true);
                    }
                    current = borrowed.prototype_or(&self.intrinsics);
                }
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn get_super(
        &mut self,
        home: &Value,
        this: &Value,
        key: &str,
    ) -> Result<Value, ScriptError> {
        let Value::Object(home) = home else {
            return Err(self.reference_error("'super' keyword unexpected here"));
        };
        let prototype = home.borrow().prototype_or(&self.intrinsics);
        match prototype {
            Some(prototype) => self.ordinary_get(prototype, key, this),
            None => Ok(Value::Undefined),
        }
    }

    pub(super) fn get_private(&mut self, object: &Value, key: &str) -> Result<Value, ScriptError> {
        let slot = match object {
            Value::Object(handle) => handle
                .borrow()
                .property(key)
                .map(|property| property.slot.clone()),
            _ => None,
        };
        match slot {
            Some(Slot::Data(value)) => Ok(value),
            Some(Slot::Accessor(accessors)) => match accessors.get {
                Some(getter) => self.call_function(getter, object.clone(), Vec::new()),
                None => Err(self.type_error(format!(
                    "'{}' was defined without a getter",
                    display_key(key)
                ))),
            },
            None => Err(self.type_error(format!(
                "Cannot read private member {} from an object whose class did not declare it",
                display_key(key)
            ))),
        }
    }

    pub(super) fn set_private(
        &mut self,
        object: &Value,
        key: &str,
        value: Value,
    ) -> Result<(), ScriptError> {
        let Value::Object(handle) = object else {
            return Err(self.type_error(format!(
                "Cannot write private member {} to an object whose class did not declare it",
                display_key(key)
            )));
        };
        let slot = handle
            .borrow()
            .property(key)
            .map(|property| (property.slot.clone(), property.writable));
        match slot {
            Some((Slot::Data(_), true)) => {
                handle.borrow_mut().insert(key.to_string(), value);
                Ok(())
            }
            Some((Slot::Data(_), false)) => Err(self.type_error(format!(
                "Private method {} is not writable",
                display_key(key)
            ))),
            Some((Slot::Accessor(accessors), _)) => match accessors.set {
                Some(setter) => self
                    .call_function(setter, object.clone(), vec![value])
                    .map(|_| ()),
                None => Err(self.type_error(format!(
                    "'{}' was defined without a setter",
                    display_key(key)
                ))),
            },
            None => Err(self.type_error(format!(
                "Cannot write private member {} to an object whose class did not declare it",
                display_key(key)
            ))),
        }
    }

    // CreateDataPropertyOrThrow, used by class fields
    pub(super) fn define_own(
        &mut self,
        object: &ObjectHandle,
        key: String,
        property: Property,
    ) -> Result<(), ScriptError> {
        let (existing, extensible) = {
            let borrowed = object.borrow();
            (
                borrowed
                    .property(&key)
                    .map(|existing| existing.configurable),
                borrowed.extensible,
            )
        };
        match existing {
            Some(false) => {
                Err(self.type_error(format!("Cannot redefine property: {}", display_key(&key))))
            }
            None if !extensible => Err(self.type_error(format!(
                "Cannot define property {}, object is not extensible",
                display_key(&key)
            ))),
            _ => {
                object.borrow_mut().define(key, property);
                Ok(())
            }
        }
    }

    pub(super) fn new_plain_object(&self) -> ObjectHandle {
        new_object(JsObject::with_prototype(
            Some(self.intrinsics.object_prototype.clone()),
            ObjectClass::Ordinary,
        ))
    }

    pub(super) fn new_array(values: Vec<Value>) -> Value {
        Value::Array(super::heap::alloc_array(values.into()))
    }

    // own enumerable properties, string and symbol keyed, as object spread copies them
    pub(super) fn own_enumerable_entries(
        &mut self,
        source: &Value,
    ) -> Result<Vec<(String, Value)>, ScriptError> {
        self.enumerable_own_properties(source, true)
    }

    // keys alone are read without calling getters, as Object.keys does
    fn enumerable_own_properties(
        &mut self,
        source: &Value,
        with_values: bool,
    ) -> Result<Vec<(String, Value)>, ScriptError> {
        if let Some(object) = self.wrapper_object(source) {
            return self.enumerable_own_properties(&Value::Object(object), with_values);
        }
        match source {
            Value::Object(object) if super::reflect::proxy_of(object).is_some() => {
                let proxy = super::reflect::proxy_of(object).expect("checked above");
                let keys = self.proxy_own_keys(&proxy)?;
                let mut entries = Vec::new();
                for key in keys {
                    let enumerable = self
                        .proxy_own_property(&proxy, &key)?
                        .is_some_and(|property| property.enumerable);
                    if enumerable {
                        let value = if with_values {
                            self.proxy_get(&proxy, &key, source)?
                        } else {
                            Value::Undefined
                        };
                        entries.push((key, value));
                    }
                }
                Ok(entries)
            }
            Value::Object(object) => {
                let keys: Vec<String> = {
                    let borrowed = object.borrow();
                    let mut keys = Vec::new();
                    if let ObjectClass::String(text) = &borrowed.class {
                        keys.extend(
                            (0..super::strings::utf16_length(text)).map(|index| index.to_string()),
                        );
                    }
                    borrowed.collect_enumerable_keys(true, &mut keys);
                    keys
                };
                if keys.len() > MAX_RUNTIME_OBJECT_PROPERTIES {
                    return Err(runtime_error(
                        "object property count exceeds the deterministic limit",
                    ));
                }
                let mut entries = Vec::with_capacity(keys.len());
                for key in keys {
                    let value = if with_values {
                        self.ordinary_get(object.clone(), &key, source)?
                    } else {
                        Value::Undefined
                    };
                    entries.push((key, value));
                }
                Ok(entries)
            }
            // an array with named properties or set-apart elements answers through its keys
            // and descriptors; a plain list of elements takes the quick path below
            Value::Array(array) if !array.borrow().properties().is_empty() => {
                let mut entries = Vec::new();
                for key in self.own_property_keys(source, false)? {
                    if !self
                        .own_property_of(source, &key)?
                        .is_some_and(|property| property.enumerable)
                    {
                        continue;
                    }
                    let value = if with_values {
                        self.get_property(source, &key)?
                    } else {
                        Value::Undefined
                    };
                    entries.push((key, value));
                }
                Ok(entries)
            }
            Value::Window => {
                let mut entries = Vec::new();
                for key in self.own_property_keys(source, false)? {
                    if !self
                        .own_property_of(source, &key)?
                        .is_some_and(|property| property.enumerable)
                    {
                        continue;
                    }
                    let value = if with_values {
                        self.window_get(&key)?
                    } else {
                        Value::Undefined
                    };
                    entries.push((key, value));
                }
                Ok(entries)
            }
            Value::TypedArray(array) => {
                let mut entries: Vec<(String, Value)> = {
                    let array = array.borrow();
                    (0..super::typed::element_count(&array))
                        .map(|index| {
                            let value = with_values
                                .then(|| super::typed::read_element(&array, index))
                                .flatten()
                                .unwrap_or(Value::Undefined);
                            (index.to_string(), value)
                        })
                        .collect()
                };
                if array.borrow().object.is_some() {
                    let object = Value::Object(super::typed_object(array));
                    entries.extend(self.enumerable_own_properties(&object, with_values)?);
                }
                Ok(entries)
            }
            other => enumerable_object_properties(other.clone()),
        }
    }

    // own enumerable string keys in property order, as JSON.stringify and Object.keys see them
    pub(super) fn own_enumerable_keys(
        &mut self,
        value: &Value,
    ) -> Result<Vec<String>, ScriptError> {
        Ok(self
            .enumerable_own_properties(value, false)?
            .into_iter()
            .map(|(key, _)| key)
            .filter(|key| !super::object::is_symbol_key(key))
            .collect())
    }

    // for-in visits enumerable string keys along the prototype chain, each name once
    pub(super) fn for_in_keys(&mut self, value: &Value) -> Result<Vec<String>, ScriptError> {
        if let Some(object) = self.wrapper_object(value) {
            return self.for_in_keys(&Value::Object(object));
        }
        if let Value::Object(object) = value
            && super::reflect::proxy_of(object).is_some()
        {
            return self.own_enumerable_keys(value);
        }
        match value {
            Value::Object(object) => {
                // fast path: if no prototype in the chain has enumerable keys (the common case
                // with Object.prototype), no shadowing is possible and no HashSet is needed
                let mut proto_has_enumerable = false;
                let mut walk = object.borrow().prototype_or(&self.intrinsics);
                while let Some(proto) = walk {
                    let borrowed = proto.borrow();
                    if borrowed.has_enumerable() || matches!(borrowed.class, ObjectClass::String(_))
                    {
                        proto_has_enumerable = true;
                        break;
                    }
                    walk = borrowed.prototype_or(&self.intrinsics);
                }
                if !proto_has_enumerable {
                    let borrowed = object.borrow();
                    let mut keys = Vec::new();
                    if let ObjectClass::String(text) = &borrowed.class {
                        keys.extend(
                            (0..super::strings::utf16_length(text)).map(|index| index.to_string()),
                        );
                    }
                    borrowed.collect_enumerable_keys(false, &mut keys);
                    return Ok(keys);
                }
                let mut seen = HashSet::default();
                let mut keys = Vec::new();
                let mut current = Some(object.clone());
                while let Some(object) = current {
                    let borrowed = object.borrow();
                    if let ObjectClass::String(text) = &borrowed.class {
                        for index in 0..super::strings::utf16_length(text) {
                            let key = index.to_string();
                            if seen.insert(key.clone()) {
                                keys.push(key);
                            }
                        }
                    }
                    borrowed.visit_for_in_keys(|key, enumerable| {
                        if seen.insert(key.to_string()) && enumerable {
                            keys.push(key.to_string());
                        }
                    });
                    current = borrowed.prototype_or(&self.intrinsics);
                }
                Ok(keys)
            }
            Value::Undefined | Value::Null => Ok(Vec::new()),
            other => Ok(enumerable_object_properties(other.clone())?
                .into_iter()
                .map(|(key, _)| key)
                .collect()),
        }
    }

    pub(super) fn for_in_step(&mut self, iterator: &Value) -> Result<Option<String>, ScriptError> {
        let Value::VmIterator(state) = iterator else {
            return Ok(None);
        };
        loop {
            let (key, object) = {
                let mut state = state.borrow_mut();
                let VmIterator::Keys {
                    keys,
                    index,
                    object,
                } = &mut *state
                else {
                    return Ok(None);
                };
                let Some(key) = keys.get(*index).cloned() else {
                    return Ok(None);
                };
                *index += 1;
                (key, object.clone())
            };
            // keys deleted during the loop are skipped
            if matches!(object, Value::Object(_)) && !self.has_property(&object, &key)? {
                continue;
            }
            return Ok(Some(key));
        }
    }

    pub(super) fn arguments_object(&mut self, arguments: Vec<Value>) -> Value {
        let mut object = JsObject::with_prototype(
            Some(self.intrinsics.object_prototype.clone()),
            ObjectClass::Arguments,
        );
        let length = arguments.len();
        for (index, value) in arguments.into_iter().enumerate() {
            object.define(index.to_string(), Property::data(value));
        }
        object.define(
            "length".to_string(),
            Property::hidden(Value::Number(length as f64)),
        );
        object.define(
            "@@iterator".to_string(),
            Property::hidden(self.intrinsics.array_values.clone()),
        );
        Value::Object(new_object(object))
    }

    pub(super) fn template_object(&mut self, code: &Rc<compiler::Code>, index: usize) -> Value {
        let key = (Rc::as_ptr(code) as usize, index);
        if let Some((_, value)) = self.template_cache.get(&key) {
            return value.clone();
        }
        let site = &code.templates[index];
        let cooked = site
            .cooked
            .iter()
            .map(|part| {
                part.as_ref().map_or(Value::Undefined, |part| {
                    Value::String(part.to_string().into())
                })
            })
            .collect();
        let raw = site
            .raw
            .iter()
            .map(|part| Value::String(part.to_string().into()))
            .collect();
        let strings = Self::new_array(cooked);
        let raw = Self::new_array(raw);
        if let Value::Array(array) = &strings {
            self.set_array_expando(array, "raw", raw);
        }
        self.template_cache
            .insert(key, (code.clone(), strings.clone()));
        strings
    }

    pub(super) fn dynamic_import_promise(
        &mut self,
        base_url: Option<AppUrl>,
        specifier: Value,
        options: Value,
    ) -> Value {
        let outcome = match base_url {
            None => Err(runtime_error("dynamic import requires a document URL")),
            Some(base_url) => {
                let attributes = Some(&options).filter(|value| !matches!(value, Value::Undefined));
                self.dynamic_import(base_url, &specifier, attributes)
            }
        };
        Value::Promise(match outcome {
            Ok(namespace) => {
                let namespace = self.namespace_object(&namespace);
                self.fulfilled_promise(namespace)
            }
            Err(error) => {
                let reason = self.take_thrown(error);
                self.rejected_promise_value(reason)
            }
        })
    }

    pub(super) fn namespace_object(&self, namespace: &ModuleNamespace) -> Value {
        let mut object = JsObject::with_prototype(None, ObjectClass::Ordinary);
        for (name, value) in &namespace.values {
            object.define(
                name.clone(),
                Property {
                    slot: Slot::Data(value.clone()),
                    enumerable: true,
                    writable: true,
                    configurable: false,
                },
            );
        }
        object.define(
            "@@toStringTag".to_string(),
            Property {
                slot: Slot::Data(Value::String("Module".to_string().into())),
                enumerable: false,
                writable: false,
                configurable: false,
            },
        );
        object.extensible = false;
        Value::Object(new_object(object))
    }

    // ---- globals ----

    pub(super) fn lookup_global(&self, name: &str) -> Option<Value> {
        if let Some(value) = self.global.borrow().values.get(name) {
            return Some(value.resolve());
        }
        if let Ok(value) = self.member(Value::Window, name) {
            return Some(value);
        }
        // what window inherits is in scope as well: addEventListener, toString
        let mut current = Some(self.intrinsics.dom.prototype("EventTarget"));
        while let Some(object) = current {
            let borrowed = object.borrow();
            if let Some(property) = borrowed.property(name)
                && let Slot::Data(value) = &property.slot
            {
                return Some(value.resolve());
            }
            current = borrowed.prototype_or(&self.intrinsics);
        }
        None
    }

    // the getter a global accessor runs, or undefined when it has none
    pub(super) fn get_global_accessor(&mut self, name: &str) -> Option<Result<Value, ScriptError>> {
        let property = self.global.borrow().accessors.get(name).cloned()?;
        Some(match property.slot {
            Slot::Accessor(accessors) => match accessors.get {
                Some(getter) => self.call_function(getter, Value::Window, Vec::new()),
                None => Ok(Value::Undefined),
            },
            Slot::Data(value) => Ok(value),
        })
    }

    pub(super) fn get_global(&mut self, name: &str) -> Result<Value, ScriptError> {
        if let Some(value) = self.get_global_accessor(name) {
            return value;
        }
        match self.lookup_global(name) {
            Some(Value::Uninitialized) => {
                Err(self.reference_error(format!("Cannot access '{name}' before initialization")))
            }
            Some(value) => Ok(value),
            None => Err(self.reference_error(format!("{name} is not defined"))),
        }
    }

    pub(super) fn set_global(
        &mut self,
        name: &str,
        value: Value,
        strict: bool,
    ) -> Result<(), ScriptError> {
        if name == "location" {
            self.request_location_navigation(value.string_value(), false);
            return Ok(());
        }
        if self.worker_sender.is_some() && matches!(name, "onmessage" | "onerror") {
            return self.assign_member(Value::Window, name, value);
        }
        let accessor = self.global.borrow().accessors.get(name).cloned();
        if let Some(property) = accessor {
            return match property.slot {
                Slot::Accessor(accessors) if accessors.set.is_some() => self
                    .call_function(
                        accessors.set.expect("a setter was just seen"),
                        Value::Window,
                        vec![value],
                    )
                    .map(drop),
                Slot::Data(_) if property.writable => {
                    let mut global = self.global.borrow_mut();
                    if let Some(property) = global.accessors.get_mut(name) {
                        property.slot = Slot::Data(value);
                    }
                    Ok(())
                }
                Slot::Data(_) if strict => Err(self.type_error(format!(
                    "Cannot assign to read only property '{name}' of object '#<Window>'"
                ))),
                _ if strict => Err(self.type_error(format!(
                    "Cannot set property {name} of #<Window> which has only a getter"
                ))),
                _ => Ok(()),
            };
        }
        let existing = self.global.borrow().values.get(name).cloned();
        match existing {
            Some(Value::Binding(cell)) => {
                *cell.borrow_mut() = value;
                Ok(())
            }
            Some(Value::ConstBinding(_)) => {
                Err(self.type_error(format!("Assignment to constant variable '{name}'")))
            }
            Some(_) => {
                self.global
                    .borrow_mut()
                    .values
                    .insert(name.to_string(), value);
                Ok(())
            }
            None if strict && self.member(Value::Window, name).is_err() => {
                Err(self.reference_error(format!("{name} is not defined")))
            }
            None => {
                self.global
                    .borrow_mut()
                    .values
                    .insert(name.to_string(), value);
                Ok(())
            }
        }
    }

    // ---- iteration ----

    pub(super) fn iterator_result(&self, value: Value, done: bool) -> Value {
        let object = self.new_plain_object();
        {
            let mut object = object.borrow_mut();
            object.insert("value".to_string(), value);
            object.insert("done".to_string(), Value::Bool(done));
        }
        Value::Object(object)
    }

    pub(super) fn get_iterator(&mut self, value: Value) -> Result<Value, ScriptError> {
        let state = self.iterator_state(value)?;
        Ok(Value::VmIterator(Rc::new(RefCell::new(state))))
    }

    fn iterator_state(&mut self, value: Value) -> Result<VmIterator, ScriptError> {
        // taken first, so an iteration the lookup itself runs reports in its own words
        let callsite = self.iterable_callsite.take();
        let plain = self.iterates_plainly(&value)?;
        match &value {
            Value::Array(array) if plain => Ok(VmIterator::Array {
                array: array.clone(),
                index: 0,
            }),
            Value::Array(_) | Value::Object(_) => {
                let method = self.get_property(&value, "@@iterator")?;
                if !is_callable(&method) {
                    return Err(self.not_iterable(callsite, &value));
                }
                let iterator = self.call_function(method, value.clone(), Vec::new())?;
                if is_primitive(&iterator) {
                    return Err(
                        self.type_error("Result of the Symbol.iterator method is not an object")
                    );
                }
                let next = self.get_property(&iterator, "next")?;
                Ok(VmIterator::Object { iterator, next })
            }
            Value::Undefined | Value::Null => Err(self.not_iterable(callsite, &value)),
            Value::ClassList(handle) => Ok(VmIterator::List {
                values: self.class_tokens(handle),
                index: 0,
            }),
            other => match host_iterable_values(other) {
                Some(values) => Ok(VmIterator::List { values, index: 0 }),
                None => Err(self.not_iterable(callsite, other)),
            },
        }
    }

    // v8's wording: the source where the call printer found it, otherwise the value's type
    fn not_iterable(&mut self, callsite: Option<Callsite>, value: &Value) -> ScriptError {
        let nullish = matches!(value, Value::Undefined | Value::Null);
        self.type_error(match callsite {
            Some(Callsite::Source(message)) => message.to_string(),
            Some(Callsite::Argument(site)) if nullish => {
                format!("{site} is not iterable (cannot read property null)")
            }
            Some(Callsite::Argument(_)) => {
                "Spread syntax requires ...iterable[Symbol.iterator] to be a function".to_string()
            }
            None => {
                let shown = match value {
                    Value::Null => " null".to_string(),
                    Value::Bool(value) => format!(" {value}"),
                    Value::Number(value) => format!(" {}", number_to_string(*value)),
                    _ => String::new(),
                };
                format!(
                    "{}{shown} is not iterable (cannot read property Symbol(Symbol.iterator))",
                    self.type_of(value)
                )
            }
        })
    }

    pub(super) fn get_async_iterator(&mut self, value: Value) -> Result<Value, ScriptError> {
        if matches!(value, Value::Object(_)) {
            let method = self.get_property(&value, "@@asyncIterator")?;
            if !matches!(method, Value::Undefined | Value::Null) {
                if !is_callable(&method) {
                    return Err(self.type_error("Symbol.asyncIterator is not a function"));
                }
                let iterator = self.call_function(method, value.clone(), Vec::new())?;
                if is_primitive(&iterator) {
                    return Err(self
                        .type_error("Result of the Symbol.asyncIterator method is not an object"));
                }
                let next = self.get_property(&iterator, "next")?;
                return Ok(Value::VmIterator(Rc::new(RefCell::new(
                    VmIterator::Object { iterator, next },
                ))));
            }
        }
        let state = self.iterator_state(value)?;
        Ok(Value::VmIterator(Rc::new(RefCell::new(
            VmIterator::AsyncFromSync(Box::new(state)),
        ))))
    }

    pub(super) fn iterator_step(&mut self, iterator: &Value) -> Result<Option<Value>, ScriptError> {
        let Value::VmIterator(state) = iterator else {
            return Err(self.type_error("value is not an iterator"));
        };
        step_state(self, state)
    }

    pub(super) fn iterator_close(&mut self, iterator: &Value) -> Result<(), ScriptError> {
        let Value::VmIterator(state) = iterator else {
            return Ok(());
        };
        let previous = std::mem::replace(&mut *state.borrow_mut(), VmIterator::Done);
        let target = match previous {
            VmIterator::Object { iterator, .. } => iterator,
            VmIterator::AsyncFromSync(inner) => match *inner {
                VmIterator::Object { iterator, .. } => iterator,
                _ => return Ok(()),
            },
            _ => return Ok(()),
        };
        let method = self.get_property(&target, "return")?;
        if matches!(method, Value::Undefined | Value::Null) {
            return Ok(());
        }
        let result = self.call_function(method, target, Vec::new())?;
        if is_primitive(&result) {
            return Err(self.type_error("iterator result is not an object"));
        }
        Ok(())
    }

    // the next result of a for-await loop, which the loop then awaits
    pub(super) fn async_iterator_next(&mut self, iterator: &Value) -> Result<Value, ScriptError> {
        let Value::VmIterator(state) = iterator else {
            return Err(self.type_error("value is not an async iterator"));
        };
        let object = match &*state.borrow() {
            VmIterator::Object { iterator, next } => Some((iterator.clone(), next.clone())),
            _ => None,
        };
        if let Some((iterator, next)) = object {
            return self.call_function(next, iterator, Vec::new());
        }
        let stepped = {
            let mut borrowed = state.borrow_mut();
            match &mut *borrowed {
                VmIterator::AsyncFromSync(inner) => {
                    let inner = std::mem::replace(&mut **inner, VmIterator::Done);
                    drop(borrowed);
                    let holder = Rc::new(RefCell::new(inner));
                    let result = step_state(self, &holder);
                    let inner = std::mem::replace(&mut *holder.borrow_mut(), VmIterator::Done);
                    if let VmIterator::AsyncFromSync(slot) = &mut *state.borrow_mut() {
                        **slot = inner;
                    }
                    result?
                }
                _ => None,
            }
        };
        match stepped {
            Some(Value::Promise(promise)) => {
                let wrap = Value::Builtin(Builtin::native(Native::IterResultWrap(false)));
                self.promise_then(promise, Some(&wrap), None)
            }
            Some(value) => Ok(self.iterator_result(value, false)),
            None => Ok(self.iterator_result(Value::Undefined, true)),
        }
    }

    // one step of a synchronous yield*, forwarding next, throw or return to the inner iterator
    pub(super) fn delegate_resume(
        &mut self,
        iterator: &Value,
        mode: ResumeMode,
        sent: Value,
    ) -> Result<DelegateResult, ScriptError> {
        let Value::VmIterator(state) = iterator else {
            return Err(self.type_error("yield* needs an iterator"));
        };
        let object = match &*state.borrow() {
            VmIterator::Object { iterator, next } => Some((iterator.clone(), next.clone())),
            _ => None,
        };
        let Some((target, next)) = object else {
            return match mode {
                ResumeMode::Next => Ok(match step_state(self, state)? {
                    Some(value) => DelegateResult::Yielded(value),
                    None => DelegateResult::Done(Value::Undefined),
                }),
                ResumeMode::Return => Ok(DelegateResult::Returned(sent)),
                ResumeMode::Throw => {
                    *state.borrow_mut() = VmIterator::Done;
                    Err(self.type_error("The iterator does not provide a 'throw' method"))
                }
            };
        };
        let result = match mode {
            ResumeMode::Next => self.call_function(next, target, vec![sent])?,
            ResumeMode::Throw => {
                let method = self.get_property(&target, "throw")?;
                if matches!(method, Value::Undefined | Value::Null) {
                    self.iterator_close(iterator)?;
                    return Err(self.type_error("The iterator does not provide a 'throw' method"));
                }
                self.call_function(method, target, vec![sent])?
            }
            ResumeMode::Return => {
                let method = self.get_property(&target, "return")?;
                if matches!(method, Value::Undefined | Value::Null) {
                    return Ok(DelegateResult::Returned(sent));
                }
                self.call_function(method, target, vec![sent])?
            }
        };
        if is_primitive(&result) {
            return Err(self.type_error("iterator result is not an object"));
        }
        let done = self.get_property(&result, "done")?.truthy();
        let value = self.get_property(&result, "value")?;
        Ok(match (done, mode) {
            (false, _) => DelegateResult::Yielded(value),
            (true, ResumeMode::Return) => DelegateResult::Returned(value),
            (true, _) => DelegateResult::Done(value),
        })
    }

    // the async form of a yield* step: calls the method and leaves its promise to be awaited
    pub(super) fn delegate_call(
        &mut self,
        iterator: &Value,
        mode: ResumeMode,
        sent: Value,
    ) -> Result<Value, ScriptError> {
        let Value::VmIterator(state) = iterator else {
            return Err(self.type_error("yield* needs an async iterator"));
        };
        let object = match &*state.borrow() {
            VmIterator::Object { iterator, next } => Some((iterator.clone(), next.clone())),
            _ => None,
        };
        let Some((target, next)) = object else {
            return match mode {
                ResumeMode::Next => self.async_iterator_next(iterator),
                ResumeMode::Return => Ok(self.iterator_result(sent, true)),
                ResumeMode::Throw => {
                    Err(self.type_error("The iterator does not provide a 'throw' method"))
                }
            };
        };
        match mode {
            ResumeMode::Next => self.call_function(next, target, vec![sent]),
            ResumeMode::Throw => {
                let method = self.get_property(&target, "throw")?;
                if matches!(method, Value::Undefined | Value::Null) {
                    self.iterator_close(iterator)?;
                    return Err(self.type_error("The iterator does not provide a 'throw' method"));
                }
                self.call_function(method, target, vec![sent])
            }
            ResumeMode::Return => {
                let method = self.get_property(&target, "return")?;
                if matches!(method, Value::Undefined | Value::Null) {
                    return Ok(self.iterator_result(sent, true));
                }
                self.call_function(method, target, vec![sent])
            }
        }
    }

    // walks an iterable one value at a time; when the visitor fails, the iterator is closed
    // and the visitor's error wins, so a throwing return method cannot mask it
    pub(super) fn for_each_iterated(
        &mut self,
        iterable: Value,
        mut visit: impl FnMut(&mut Self, Value) -> Result<(), ScriptError>,
    ) -> Result<(), ScriptError> {
        let iterator = self.get_iterator(iterable)?;
        while let Some(item) = self.iterator_step(&iterator)? {
            if let Err(error) = visit(self, item) {
                let thrown = self.pending_exception.take();
                if self.iterator_close(&iterator).is_err() {
                    self.pending_exception = None;
                }
                self.pending_exception = thrown;
                return Err(error);
            }
        }
        Ok(())
    }

    // whether an array still iterates through the built-in values(), so its elements can be
    // read directly instead of through a replaced Symbol.iterator
    pub(super) fn iterates_plainly(&mut self, value: &Value) -> Result<bool, ScriptError> {
        if !matches!(value, Value::Array(_)) {
            return Ok(false);
        }
        let method = self.get_property(value, "@@iterator")?;
        Ok(
            matches!(&method, Value::Object(object) if matches!(object.borrow().class, ObjectClass::Native(ref native) if matches!(**native, Native::ArrayValues))),
        )
    }

    pub(super) fn iterate_to_vec(&mut self, value: Value) -> Result<Vec<Value>, ScriptError> {
        match &value {
            Value::Array(array) if self.iterates_plainly(&value)? => {
                return Ok(array.borrow().iter().cloned().map(Value::filled).collect());
            }
            Value::Array(_) => {}
            other if !matches!(other, Value::Object(_)) => {
                if let Some(values) = host_iterable_values(other) {
                    return Ok(values);
                }
            }
            _ => {}
        }
        let iterator = self.get_iterator(value)?;
        let mut values = Vec::new();
        while let Some(value) = self.iterator_step(&iterator)? {
            if values.len() == MAX_USER_ITERATOR_VALUES {
                return Err(
                    self.range_error("user-defined iterator exceeded the deterministic limit")
                );
            }
            values.push(value);
        }
        Ok(values)
    }
}

fn step_state(
    runtime: &mut ScriptRuntime,
    state: &Rc<RefCell<VmIterator>>,
) -> Result<Option<Value>, ScriptError> {
    let object = {
        let mut borrowed = state.borrow_mut();
        match &mut *borrowed {
            VmIterator::Array { array, index } => {
                let value = array.borrow().get(*index).cloned().map(Value::filled);
                return Ok(match value {
                    Some(value) => {
                        *index += 1;
                        Some(value)
                    }
                    None => {
                        *borrowed = VmIterator::Done;
                        None
                    }
                });
            }
            VmIterator::List { values, index } => {
                let value = values.get(*index).cloned();
                return Ok(match value {
                    Some(value) => {
                        *index += 1;
                        Some(value)
                    }
                    None => {
                        *borrowed = VmIterator::Done;
                        None
                    }
                });
            }
            VmIterator::Object { iterator, next } => (iterator.clone(), next.clone()),
            VmIterator::Keys { .. } | VmIterator::AsyncFromSync(_) | VmIterator::Done => {
                return Ok(None);
            }
        }
    };
    let (iterator, next) = object;
    let result = runtime.call_function(next, iterator, Vec::new())?;
    if is_primitive(&result) {
        return Err(runtime.type_error("iterator result is not an object"));
    }
    if runtime.get_property(&result, "done")?.truthy() {
        *state.borrow_mut() = VmIterator::Done;
        return Ok(None);
    }
    runtime.get_property(&result, "value").map(Some)
}

// own properties computed from an object's internal slot rather than stored
fn exotic_own(object: &JsObject, key: &str) -> Option<Value> {
    match &object.class {
        ObjectClass::String(text) => {
            if key == "length" {
                return Some(Value::Number(super::strings::utf16_length(text) as f64));
            }
            let index = array_index(key)?;
            super::strings::unit_at(text, index as usize)
        }
        _ => None,
    }
}

fn receiver_label(value: &Value) -> String {
    match value {
        Value::Object(object) => match &object.borrow().class {
            ObjectClass::Function(closure) => format!("function '{}'", closure.code.name),
            _ => "object".to_string(),
        },
        Value::Array(_) => "array".to_string(),
        other => other.string_value(),
    }
}

#[inline]
pub(super) fn strict_equals(left: &Value, right: &Value) -> bool {
    values_equal(left, right)
}

// SameValueZero: like === except NaN equals itself
pub(super) fn same_value_zero(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) if left.is_nan() && right.is_nan() => true,
        _ => strict_equals(left, right),
    }
}

pub(super) fn same_value(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => {
            (left.is_nan() && right.is_nan())
                || (left == right && left.is_sign_negative() == right.is_sign_negative())
        }
        _ => strict_equals(left, right),
    }
}

fn power(base: f64, exponent: f64) -> f64 {
    if exponent.is_nan() || (base.abs() == 1.0 && exponent.is_infinite()) {
        return f64::NAN;
    }
    base.powf(exponent)
}

// instanceof against the host constructors that are not function objects
fn host_instance_of(value: &Value, constructor: &Value) -> bool {
    matches!(
        (value, constructor),
        (Value::Blob(_), Value::BlobConstructor)
            | (
                Value::File(_),
                Value::FileConstructor | Value::BlobConstructor
            )
            | (Value::Worker(_), Value::WorkerConstructor)
            | (Value::Response(_), Value::ResponseConstructor)
            | (Value::Request(_), Value::RequestConstructor)
            | (Value::Headers(_), Value::HeadersConstructor)
            | (Value::FormData(_), Value::FormDataConstructor)
            | (Value::AbortController(_), Value::AbortControllerConstructor)
    )
}

// the values a host collection yields when iterated
fn host_iterable_values(value: &Value) -> Option<Vec<Value>> {
    let pair = |first: Value, second: Value| {
        Value::Array(super::heap::alloc_array(vec![first, second].into()))
    };
    Some(match value {
        Value::Array(values) => values.borrow().iter().cloned().map(Value::filled).collect(),
        Value::NodeList(nodes) => nodes.iter().cloned().map(Value::Element).collect(),
        Value::String(string) => string
            .chars()
            .map(|character| Value::String(character.to_string().into()))
            .collect(),
        Value::TypedArray(array) => super::typed::elements(&array.borrow()),
        Value::Iterator(iterator) => {
            let mut iterator = iterator.borrow_mut();
            let values = iterator.values[iterator.index..].to_vec();
            iterator.index = iterator.values.len();
            values
        }
        Value::FormData(form_data) => form_data
            .borrow()
            .pairs
            .iter()
            .map(|(name, value)| {
                pair(
                    Value::String(name.clone().into()),
                    form_data_entry_value(value),
                )
            })
            .collect(),
        Value::FileList(files) => files.iter().cloned().map(Value::File).collect(),
        Value::Headers(headers) => headers
            .borrow()
            .values
            .iter()
            .map(|(name, value)| {
                pair(
                    Value::String(name.clone().into()),
                    Value::String(value.clone().into()),
                )
            })
            .collect(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_format_like_number_to_string() {
        for (value, expected) in [
            (0.1 + 0.2, "0.30000000000000004"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123.456, "123.456"),
            (1e-7, "1e-7"),
            (0.000001, "0.000001"),
            (-0.0, "0"),
            (2.5e-10, "2.5e-10"),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            (42.0, "42"),
            (-1.5, "-1.5"),
        ] {
            assert_eq!(number_to_string(value), expected);
        }
    }

    #[test]
    fn values_stay_small_enough_to_move_cheaply() {
        // the VM copies values on and off its stack constantly
        assert!(
            std::mem::size_of::<Value>() <= 32,
            "{}",
            std::mem::size_of::<Value>()
        );
    }

    #[test]
    fn strings_convert_to_numbers_like_the_spec() {
        assert_eq!(string_to_number("  12.5\n"), 12.5);
        assert_eq!(string_to_number(""), 0.0);
        assert_eq!(string_to_number("0x1F"), 31.0);
        assert_eq!(string_to_number("0b101"), 5.0);
        assert_eq!(string_to_number("-Infinity"), f64::NEG_INFINITY);
        assert_eq!(string_to_number(".5"), 0.5);
        assert!(string_to_number("inf").is_nan());
        assert!(string_to_number("12px").is_nan());
        assert!(string_to_number("-0x10").is_nan());
        assert_eq!(to_int32_number(4_294_967_295.0), -1);
        assert_eq!(to_uint32_number(-1.0), 4_294_967_295);
    }
}
