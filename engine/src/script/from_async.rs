// Array.fromAsync: reads an async iterable, a sync iterable or an array-like one value at a
// time, awaiting each value (and each mapped value) on the promise queue before the next

use super::operations::{is_callable, is_primitive};
use super::promises::PromiseFunction;
use super::realm::Native;
use super::*;

enum Source {
    // an async iterator and its next method
    Async(Value, Value),
    // a sync iterator and its next method, whose values are awaited
    Sync(Value, Value),
    ArrayLike(Value, u64),
}

pub(super) struct FromAsync {
    result: PromiseHandle,
    target: Value,
    source: Source,
    mapper: Value,
    this_arg: Value,
    index: RefCell<u64>,
}

impl FromAsync {
    pub(super) fn trace(self: &Rc<Self>, tracer: &mut super::heap::Tracer) {
        if !tracer.visit_rc(self) {
            return;
        }
        tracer.trace_promise(&self.result);
        tracer.trace_value(&self.target);
        match &self.source {
            Source::Async(iter, next) | Source::Sync(iter, next) => {
                tracer.trace_value(iter);
                tracer.trace_value(next);
            }
            Source::ArrayLike(items, _) => tracer.trace_value(items),
        }
        tracer.trace_value(&self.mapper);
        tracer.trace_value(&self.this_arg);
    }
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
        Value::builtin(Builtin::native(Native::Promise(Rc::new(
            PromiseFunction::FromAsyncStep(state.clone(), phase),
        ))))
    }

    pub(super) fn array_from_async(&mut self, this: Value, arguments: Vec<Value>) -> Value {
        let result = Self::pending_promise();
        if let Err(error) = self.start_from_async(&result, this, arguments) {
            let reason = self.take_thrown(error);
            self.settle_promise(&result, PromiseOutcome::Rejected(reason));
        }
        Value::Promise(result)
    }

    fn start_from_async(
        &mut self,
        result: &PromiseHandle,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<(), ScriptError> {
        let mut arguments = arguments.into_iter();
        let items = arguments.next().unwrap_or(Value::Undefined).resolve();
        let mapper = arguments.next().unwrap_or(Value::Undefined);
        let this_arg = arguments.next().unwrap_or(Value::Undefined);
        if !matches!(mapper, Value::Undefined) && !is_callable(&mapper) {
            return Err(self.type_error("Array.fromAsync mapper is not a function"));
        }
        if matches!(items, Value::Undefined | Value::Null) {
            return Err(self.type_error("Array.fromAsync requires an iterable or array-like value"));
        }
        let async_method = self.get_property(&items, "@@asyncIterator")?;
        let source =
            if !matches!(async_method, Value::Undefined | Value::Null) {
                if !is_callable(&async_method) {
                    return Err(self.type_error("Symbol.asyncIterator is not a function"));
                }
                let iterator = self.call_function(async_method, items, Vec::new())?;
                if is_primitive(&iterator) {
                    return Err(self
                        .type_error("Result of the Symbol.asyncIterator method is not an object"));
                }
                let next = self.get_property(&iterator, "next")?;
                Source::Async(iterator, next)
            } else {
                let sync_method = self.get_property(&items, "@@iterator")?;
                if !matches!(sync_method, Value::Undefined | Value::Null) {
                    if !is_callable(&sync_method) {
                        return Err(self.type_error("Symbol.iterator is not a function"));
                    }
                    let iterator = self.call_function(sync_method, items, Vec::new())?;
                    if is_primitive(&iterator) {
                        return Err(self
                            .type_error("Result of the Symbol.iterator method is not an object"));
                    }
                    let next = self.get_property(&iterator, "next")?;
                    Source::Sync(iterator, next)
                } else {
                    let array_like = self.coerce_object(items)?;
                    let length = self.array_like_length(&array_like)?;
                    Source::ArrayLike(array_like, length)
                }
            };
        let target = match &source {
            Source::Async(..) | Source::Sync(..) => {
                if Self::is_constructor(&this) {
                    self.construct(this, Vec::new(), None)?
                } else {
                    Self::new_array(Vec::new())
                }
            }
            Source::ArrayLike(_, length) => {
                if Self::is_constructor(&this) {
                    self.construct(this, vec![Value::Number(*length as f64)], None)?
                } else {
                    if *length > u32::MAX as u64 {
                        return Err(self.range_error("Invalid array length"));
                    }
                    if *length > MAX_RUNTIME_ARRAY_LENGTH as u64 {
                        return Err(
                            self.range_error("array exceeds the deterministic length limit")
                        );
                    }
                    Self::new_array(vec![Value::Uninitialized; *length as usize])
                }
            }
        };
        let state = Rc::new(FromAsync {
            result: result.clone(),
            target,
            source,
            mapper,
            this_arg,
            index: RefCell::new(0),
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
            Source::Sync(iterator, next) => {
                let result = self.call_function(next.clone(), iterator.clone(), Vec::new())?;
                if is_primitive(&result) {
                    return Err(self.type_error("Iterator result is not an object"));
                }
                if self.get_property(&result, "done")?.truthy() {
                    return self.finish_from_async(state);
                }
                let value = self.get_property(&result, "value")?;
                (value, Phase::Value, true)
            }
            Source::ArrayLike(items, length) => {
                let index = *state.index.borrow();
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
        let length = *state.index.borrow();
        self.set_property(&state.target, "length", Value::Number(length as f64), true)?;
        self.settle_promise(
            &state.result,
            PromiseOutcome::Fulfilled(state.target.clone()),
        );
        Ok(())
    }

    fn close_from_async(&mut self, state: &Rc<FromAsync>) {
        let closed = match &state.source {
            Source::Sync(iterator, _) => self.close_iterator_object(iterator),
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

    fn write_from_async(&mut self, state: &Rc<FromAsync>, value: Value) -> Result<(), ScriptError> {
        let index = *state.index.borrow();
        if let Err(error) =
            self.create_data_property_or_throw(&state.target, index.to_string(), value)
        {
            let reason = self.take_thrown(error);
            self.close_from_async(state);
            return Err(self.throw_value(reason));
        }
        *state.index.borrow_mut() = index + 1;
        self.advance_from_async(state)
    }

    // a value is in hand: map it and wait, or keep it and move on
    fn take_from_async(&mut self, state: &Rc<FromAsync>, value: Value) -> Result<(), ScriptError> {
        if matches!(state.mapper, Value::Undefined) {
            return self.write_from_async(state, value);
        }
        let index = Value::Number(*state.index.borrow() as f64);
        match self.call_function(
            state.mapper.clone(),
            state.this_arg.clone(),
            vec![value, index],
        ) {
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
            Phase::Mapped => self.write_from_async(state, settled),
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
