// Atomics over integer typed arrays: with no SharedArrayBuffer every buffer belongs to one
// agent, so the operations are plain reads and writes in the spec's order of checks, notify
// wakes nobody and wait refuses, as browsers do for an unshared buffer

use super::typed::{ElementKind, typed_range};
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AtomicsOp {
    Add,
    And,
    CompareExchange,
    Exchange,
    IsLockFree,
    Load,
    Or,
    Store,
    Sub,
    Xor,
    Notify,
    Wait,
    WaitAsync,
    Pause,
}

pub(super) const ATOMICS_METHODS: &[(&str, AtomicsOp, u32)] = &[
    ("add", AtomicsOp::Add, 3),
    ("and", AtomicsOp::And, 3),
    ("compareExchange", AtomicsOp::CompareExchange, 4),
    ("exchange", AtomicsOp::Exchange, 3),
    ("isLockFree", AtomicsOp::IsLockFree, 1),
    ("load", AtomicsOp::Load, 2),
    ("or", AtomicsOp::Or, 3),
    ("store", AtomicsOp::Store, 3),
    ("sub", AtomicsOp::Sub, 3),
    ("xor", AtomicsOp::Xor, 3),
    ("notify", AtomicsOp::Notify, 3),
    ("wait", AtomicsOp::Wait, 4),
    ("waitAsync", AtomicsOp::WaitAsync, 4),
    ("pause", AtomicsOp::Pause, 0),
];

fn little_word(bytes: &[u8]) -> u64 {
    let mut word = [0; 8];
    word[..bytes.len()].copy_from_slice(bytes);
    u64::from_le_bytes(word)
}

impl ScriptRuntime {
    // ValidateIntegerTypedArray: wait and notify take only Int32Array and BigInt64Array
    fn atomic_array(
        &mut self,
        value: &Value,
        waitable: bool,
    ) -> Result<TypedArrayHandle, ScriptError> {
        let Value::TypedArray(array) = value else {
            return Err(self.type_error("Atomics operations require an integer typed array"));
        };
        let kind = array.borrow().kind;
        let allowed = if waitable {
            matches!(kind, ElementKind::Int32 | ElementKind::BigInt64)
        } else {
            !matches!(
                kind,
                ElementKind::Uint8Clamped
                    | ElementKind::Float16
                    | ElementKind::Float32
                    | ElementKind::Float64
            )
        };
        if !allowed {
            return Err(self.type_error(format!(
                "Atomics operations are not allowed on a {}",
                kind.name()
            )));
        }
        self.atomic_bounds(array)?;
        Ok(array.clone())
    }

    fn atomic_bounds(&mut self, array: &TypedArrayHandle) -> Result<(usize, usize), ScriptError> {
        match typed_range(&array.borrow()) {
            Some(range) => Ok(range),
            None => Err(self.type_error(
                "Atomics operations need a typed array whose buffer is attached and in bounds",
            )),
        }
    }

    // ValidateAtomicAccess: the element's index, checked against the current length
    fn atomic_index(
        &mut self,
        array: &TypedArrayHandle,
        index: Value,
    ) -> Result<usize, ScriptError> {
        let number = match index {
            Value::Undefined => 0.0,
            other => self.coerce_number(other)?,
        };
        let number = if number.is_nan() { 0.0 } else { number.trunc() };
        let length = super::typed::element_count(&array.borrow());
        if number < 0.0 || number >= length as f64 {
            return Err(self.range_error("Invalid atomic access index"));
        }
        Ok(number as usize)
    }

    // ToBigInt for the 64-bit kinds, otherwise ToIntegerOrInfinity
    fn atomic_operand(&mut self, kind: ElementKind, value: Value) -> Result<Value, ScriptError> {
        if kind.is_bigint() {
            return Ok(Value::BigInt(self.coerce_bigint(value)?));
        }
        let number = self.coerce_number(value)?;
        Ok(Value::Number(if number.is_nan() {
            0.0
        } else {
            number.trunc() + 0.0
        }))
    }

    pub(super) fn call_atomics(
        &mut self,
        op: AtomicsOp,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        match op {
            AtomicsOp::IsLockFree => {
                let size = self.coerce_number(argument(0))?;
                return Ok(Value::Bool(
                    matches!(size as i64, 1 | 2 | 4 | 8) && size.fract() == 0.0,
                ));
            }
            AtomicsOp::Pause => {
                let count = argument(0);
                if !matches!(&count, Value::Undefined)
                    && !matches!(&count, Value::Number(number) if number.is_finite() && number.fract() == 0.0)
                {
                    return Err(self.type_error("Atomics.pause takes an integral number"));
                }
                std::hint::spin_loop();
                return Ok(Value::Undefined);
            }
            _ => {}
        }
        let waitable = matches!(
            op,
            AtomicsOp::Notify | AtomicsOp::Wait | AtomicsOp::WaitAsync
        );
        let array = self.atomic_array(&argument(0), waitable)?;
        let index = self.atomic_index(&array, argument(1))?;
        let kind = array.borrow().kind;
        match op {
            AtomicsOp::Notify => {
                if !matches!(argument(2), Value::Undefined) {
                    self.coerce_number(argument(2))?;
                }
                // an unshared buffer has no other agent waiting on it
                return Ok(Value::Number(0.0));
            }
            AtomicsOp::Wait | AtomicsOp::WaitAsync => {
                return Err(self.type_error(
                    "Atomics.wait and Atomics.waitAsync need a typed array over a SharedArrayBuffer",
                ));
            }
            AtomicsOp::Load => {
                self.atomic_bounds(&array)?;
                return Ok(
                    super::typed::read_element(&array.borrow(), index).unwrap_or(Value::Undefined)
                );
            }
            _ => {}
        }
        let operand = self.atomic_operand(kind, argument(2))?;
        let replacement = if op == AtomicsOp::CompareExchange {
            Some(self.atomic_operand(kind, argument(3))?)
        } else {
            None
        };
        // the conversions may have detached or shrunk the buffer
        let (start, _) = self.atomic_bounds(&array)?;
        if index >= super::typed::element_count(&array.borrow()) {
            return Err(self.range_error("Invalid atomic access index"));
        }
        let size = kind.size();
        let offset = start + index * size;
        let buffer = array.borrow().buffer.clone();
        let mut buffer = buffer.borrow_mut();
        let slot = &mut buffer[offset..offset + size];
        let old = slot.to_vec();
        let encoded = kind.encode_value(&operand);
        let new = match op {
            AtomicsOp::Store => {
                slot.copy_from_slice(&encoded);
                return Ok(operand);
            }
            AtomicsOp::Exchange => encoded,
            AtomicsOp::CompareExchange => {
                if old == encoded {
                    kind.encode_value(&replacement.expect("read above"))
                } else {
                    old.clone()
                }
            }
            _ => {
                let (left, right) = (little_word(&old), little_word(&encoded));
                let word = match op {
                    AtomicsOp::Add => left.wrapping_add(right),
                    AtomicsOp::Sub => left.wrapping_sub(right),
                    AtomicsOp::And => left & right,
                    AtomicsOp::Or => left | right,
                    _ => left ^ right,
                };
                word.to_le_bytes()[..size].to_vec()
            }
        };
        slot.copy_from_slice(&new);
        Ok(kind.read_value(&old))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomics_read_modify_write_integer_arrays() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                const bytes = new Uint8Array(4);
                seen.push(Atomics.add(bytes, 0, 300), bytes[0], Atomics.sub(bytes, 1, 1), bytes[1]);
                seen.push(Atomics.store(bytes, 2, -0), Atomics.store(bytes, 2, 7.9), Atomics.load(bytes, 2));
                seen.push(Atomics.compareExchange(bytes, 2, 7, 9), Atomics.compareExchange(bytes, 2, 1, 5), bytes[2]);
                seen.push(Atomics.exchange(bytes, 3, 6), Atomics.or(bytes, 3, 1), Atomics.xor(bytes, 3, 7), Atomics.and(bytes, 3, 1), bytes[3]);
                const big = new BigInt64Array(1);
                seen.push(Atomics.add(big, 0, 5n), Atomics.load(big, 0), Atomics.notify(new Int32Array(1), 0));
                seen.push(Atomics.isLockFree(4), Atomics.isLockFree(3), Object.prototype.toString.call(Atomics));
                for (const call of [() => Atomics.add(new Float64Array(1), 0, 1), () => Atomics.load(bytes, 4),
                                    () => Atomics.wait(new Int32Array(1), 0, 0), () => Atomics.notify(bytes, 0)]) {
                    try { call(); } catch (error) { seen.push(error.name); }
                }
                document.getElementById("out").textContent = seen.join("|");
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(
                "0|44|0|255|0|7|7|7|9|9|0|6|7|0|0|0|5|0|true|false|[object Atomics]|\
                 TypeError|RangeError|TypeError|TypeError"
            )
        );
    }
}
