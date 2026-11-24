// Array.fromAsync: reads an async iterable, a sync iterable or an array-like one value at a
// time, awaiting each value (and each mapped value) on the promise queue before the next

use super::operations::{is_callable, is_primitive};
use super::promises::PromiseFunction;
use super::realm::Native;
use super::*;

enum Source {
    // an async iterator and its next method
    Async(Value, Value),
    // a sync iterator from get_iterator, whose values are awaited
    Sync(Value),
    ArrayLike(Value, usize),
}

pub(super) struct FromAsync {
    result: PromiseHandle,
    source: Source,
    mapper: Value,
    values: RefCell<Vec<Value>>,
}

#[derive(Clone, Copy)]
pub(super) enum Phase {
    // an async iterator's next settled with a result object
    Result,
    // a value read from a sync iterator or an array-like settled
    Value,
    // what the mapper handed back settled
    Mapped,
    // something awaited was rejected; close the iterator when a value was in hand
    Rejected { close: bool },
}

impl ScriptRuntime {
    fn from_async_step(state: &Rc<FromAsync>, phase: Phase) -> Value {
        Value::Builtin(Builtin::native(Native::Promise(Rc::new(
            PromiseFunction::FromAsyncStep(state.clone(), phase),
        ))))
    }

    pub(super) fn array_from_async(&mut self, arguments: Vec<Value>) -> Value {
        let result = Self::pending_promise();
        if let Err(error) = self.start_from_async(&result, arguments) {
            let reason = self.take_thrown(error);
            self.settle_promise(&result, PromiseOutcome::Rejected(reason));
        }
        Value::Promise(result)
    }

    fn start_from_async(
        &mut self,
        result: &PromiseHandle,
        arguments: Vec<Value>,
    ) -> Result<(), ScriptError> {
        let mut arguments = arguments.into_iter();
        let items = arguments.next().unwrap_or(Value::Undefined).resolve();
        let mapper = arguments.next().unwrap_or(Value::Undefined);
        if !matches!(mapper, Value::Undefined) && !is_callable(&mapper) {
            return Err(self.type_error("Array.fromAsync mapper is not a function"));
        }
        if matches!(items, Value::Undefined | Value::Null) {
            return Err(self.type_error("Array.fromAsync requires an iterable or array-like value"));
        }
        let async_method = if is_primitive(&items) {
            Value::Undefined
        } else {
            self.get_property(&items, "@@asyncIterator")?
        };
        let source = if !matches!(async_method, Value::Undefined | Value::Null) {
            let iterator = self.call_function(async_method, items, Vec::new())?;
            if is_primitive(&iterator) {
                return Err(
                    self.type_error("Result of the Symbol.asyncIterator method is not an object")
                );
            }
            let next = self.get_property(&iterator, "next")?;
            Source::Async(iterator, next)
        } else if !matches!(
            self.get_property(&items, "@@iterator")?,
            Value::Undefined | Value::Null
        ) {
            Source::Sync(self.get_iterator(items)?)
        } else {
            let length = self.get_property(&items, "length")?;
            let length = self.coerce_number(length)?;
            let length = if length.is_nan() || length <= 0.0 {
                0
            } else {
                length.min(MAX_RUNTIME_ARRAY_LENGTH as f64) as usize
            };
            Source::ArrayLike(items, length)
        };
        let state = Rc::new(FromAsync {
            result: result.clone(),
            source,
            mapper,
            values: RefCell::new(Vec::new()),
        });
        self.advance_from_async(&state)
    }

    // asks the source for its next value and waits for it
    fn advance_from_async(&mut self, state: &Rc<FromAsync>) -> Result<(), ScriptError> {
        let (awaited, phase, close) = match &state.source {
            Source::Async(iterator, next) => {
                let result = self.call_function(next.clone(), iterator.clone(), Vec::new())?;
                (result, Phase::Result, false)
            }
            Source::Sync(iterator) => match self.iterator_step(iterator)? {
                Some(value) => (value, Phase::Value, true),
                None => return self.finish_from_async(state),
            },
            Source::ArrayLike(items, length) => {
                let index = state.values.borrow().len();
                if index >= *length {
                    return self.finish_from_async(state);
                }
                let value = self.get_property(items, &index.to_string())?;
                (value, Phase::Value, false)
            }
        };
        self.await_from_async(state, awaited, phase, close)
    }

    fn await_from_async(
        &mut self,
        state: &Rc<FromAsync>,
        awaited: Value,
        phase: Phase,
        close: bool,
    ) -> Result<(), ScriptError> {
        let promise = self.promise_resolve(awaited)?;
        let fulfilled = Self::from_async_step(state, phase);
        let rejected = Self::from_async_step(state, Phase::Rejected { close });
        self.promise_then(promise, Some(&fulfilled), Some(&rejected))
            .map(|_| ())
    }

    fn finish_from_async(&mut self, state: &Rc<FromAsync>) -> Result<(), ScriptError> {
        let values = std::mem::take(&mut *state.values.borrow_mut());
        self.settle_promise(
            &state.result,
            PromiseOutcome::Fulfilled(Self::new_array(values)),
        );
        Ok(())
    }

    fn close_from_async(&mut self, state: &Rc<FromAsync>) {
        let closed = match &state.source {
            Source::Sync(iterator) => self.iterator_close(iterator),
            Source::Async(iterator, _) => {
                self.get_property(iterator, "return")
                    .and_then(|close| match close {
                        Value::Undefined | Value::Null => Ok(()),
                        close => self
                            .call_function(close, iterator.clone(), Vec::new())
                            .map(|_| ()),
                    })
            }
            Source::ArrayLike(..) => Ok(()),
        };
        if let Err(error) = closed {
            self.take_thrown(error);
        }
    }

    // a value is in hand: map it and wait, or keep it and move on
    fn take_from_async(&mut self, state: &Rc<FromAsync>, value: Value) -> Result<(), ScriptError> {
        if matches!(state.mapper, Value::Undefined) {
            state.values.borrow_mut().push(value);
            return self.advance_from_async(state);
        }
        let index = Value::Number(state.values.borrow().len() as f64);
        match self.call_function(state.mapper.clone(), Value::Undefined, vec![value, index]) {
            Ok(mapped) => self.await_from_async(state, mapped, Phase::Mapped, true),
            Err(error) => {
                let reason = self.take_thrown(error);
                self.close_from_async(state);
                Err(self.throw_value(reason))
            }
        }
    }

    pub(super) fn step_from_async(
        &mut self,
        state: &Rc<FromAsync>,
        phase: Phase,
        settled: Value,
    ) -> Result<Value, ScriptError> {
        let outcome = match phase {
            Phase::Rejected { close } => {
                if close {
                    self.close_from_async(state);
                }
                self.settle_promise(&state.result, PromiseOutcome::Rejected(settled));
                return Ok(Value::Undefined);
            }
            Phase::Result => {
                if is_primitive(&settled) {
                    Err(self.type_error("Iterator result is not an object"))
                } else {
                    match self.get_property(&settled, "done") {
                        Ok(done) if done.truthy() => self.finish_from_async(state),
                        Ok(_) => match self.get_property(&settled, "value") {
                            Ok(value) => self.take_from_async(state, value),
                            Err(error) => Err(error),
                        },
                        Err(error) => Err(error),
                    }
                }
            }
            Phase::Value => self.take_from_async(state, settled),
            Phase::Mapped => {
                state.values.borrow_mut().push(settled);
                self.advance_from_async(state)
            }
        };
        if let Err(error) = outcome {
            let reason = self.take_thrown(error);
            self.settle_promise(&state.result, PromiseOutcome::Rejected(reason));
        }
        Ok(Value::Undefined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_async_awaits_each_value_in_order() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                const out = document.getElementById("out");
                async function* letters() { yield "a"; yield "b"; }
                (async () => {
                    seen.push((await Array.fromAsync(letters())).join());
                    seen.push((await Array.fromAsync([Promise.resolve(1), 2], (x) => x * 10)).join());
                    seen.push((await Array.fromAsync({ length: 2, 0: "x", 1: Promise.resolve("y") })).join());
                    seen.push((await Array.fromAsync(new Set([3]), async (x) => x + 1)).join());
                    let closed = false;
                    const iterable = { [Symbol.iterator]() { return { next: () => ({ value: 1, done: false }), return() { closed = true; return {}; } }; } };
                    try { await Array.fromAsync(iterable, () => { throw new Error("stop"); }); } catch (error) { seen.push(error.message, closed); }
                    try { await Array.fromAsync(null); } catch (error) { seen.push(error.name); }
                    try { await Array.fromAsync([Promise.reject(new RangeError("no"))]); } catch (error) { seen.push(error.name); }
                    seen.push(Array.fromAsync([]) instanceof Promise);
                    out.textContent = seen.join("|");
                })();
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some("a,b|10,20|x,y|4|stop|true|TypeError|RangeError|true")
        );
    }
}
