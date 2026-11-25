// the Iterator global and the helpers every iterator inherits: map, filter, take, drop and
// flatMap hand back a lazy iterator, and reduce, toArray, forEach, some, every and find run
// the iterator out

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, new_object};
use super::operations::{is_callable, is_primitive};
use super::realm::{Builder, Native};
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HelperMember {
    Map,
    Filter,
    Take,
    Drop,
    FlatMap,
    Reduce,
    ToArray,
    ForEach,
    Some,
    Every,
    Find,
    // the next and return of what the lazy helpers and Iterator.from hand back
    Next,
    Return,
    From,
    Constructor,
}

enum Step {
    Map(Value),
    Filter(Value),
    Take(f64),
    Drop(f64),
    // the callback, and the inner iterator being read with its next method
    FlatMap(Value, Option<(Value, Value)>),
    // Iterator.from around an iterator that does not inherit the helpers
    Wrap,
}

pub(super) struct HelperState {
    iterator: Value,
    next: Value,
    step: Step,
    counter: f64,
    done: bool,
}

impl HelperState {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        tracer.trace_value(&self.iterator);
        tracer.trace_value(&self.next);
        match &self.step {
            Step::Map(cb) | Step::Filter(cb) => tracer.trace_value(cb),
            Step::FlatMap(cb, inner) => {
                tracer.trace_value(cb);
                if let Some((iter, next)) = inner {
                    tracer.trace_value(iter);
                    tracer.trace_value(next);
                }
            }
            Step::Take(_) | Step::Drop(_) | Step::Wrap => {}
        }
    }
}

pub(super) struct HelperInterfaces {
    helper: ObjectHandle,
    wrap: ObjectHandle,
}

pub(super) fn create(
    build: &Builder,
    iterator_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> HelperInterfaces {
    let method = |target: &ObjectHandle, name: &str, member: HelperMember, length: u32| {
        let function = build.function(Native::Helper(member), name, length);
        target
            .borrow_mut()
            .define(name.to_string(), Property::hidden(Value::Object(function)));
    };
    for (name, member, length) in [
        ("map", HelperMember::Map, 1),
        ("filter", HelperMember::Filter, 1),
        ("take", HelperMember::Take, 1),
        ("drop", HelperMember::Drop, 1),
        ("flatMap", HelperMember::FlatMap, 1),
        ("reduce", HelperMember::Reduce, 1),
        ("toArray", HelperMember::ToArray, 0),
        ("forEach", HelperMember::ForEach, 1),
        ("some", HelperMember::Some, 1),
        ("every", HelperMember::Every, 1),
        ("find", HelperMember::Find, 1),
    ] {
        method(iterator_prototype, name, member, length);
    }
    let tag = |target: &ObjectHandle, name: &str| {
        target.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property::hidden(Value::String(name.into())),
        );
    };
    tag(iterator_prototype, "Iterator");
    let under = || {
        new_object(JsObject::with_prototype(
            Some(iterator_prototype.clone()),
            ObjectClass::Ordinary,
        ))
    };
    let helper = under();
    let wrap = under();
    for target in [&helper, &wrap] {
        method(target, "next", HelperMember::Next, 0);
        method(target, "return", HelperMember::Return, 0);
    }
    tag(&helper, "Iterator Helper");
    let constructor = build.constructor(
        Native::Helper(HelperMember::Constructor),
        "Iterator",
        0,
        iterator_prototype,
    );
    method(&constructor, "from", HelperMember::From, 1);
    globals.push(("Iterator", Value::Object(constructor)));
    HelperInterfaces { helper, wrap }
}

impl ScriptRuntime {
    // the next value of an iterator, none once it says it is done
    fn step_iterator(
        &mut self,
        iterator: &Value,
        next: &Value,
    ) -> Result<Option<Value>, ScriptError> {
        let result = self.call_function(next.clone(), iterator.clone(), Vec::new())?;
        if is_primitive(&result) {
            return Err(self.type_error("Iterator result is not an object"));
        }
        if self.get_property(&result, "done")?.truthy() {
            return Ok(None);
        }
        Ok(Some(self.get_property(&result, "value")?))
    }

    // tells an iterator it will not be read further
    fn close_iterator(&mut self, iterator: &Value) -> Result<(), ScriptError> {
        let close = self.get_property(iterator, "return")?;
        if !matches!(close, Value::Undefined | Value::Null) {
            self.call_function(close, iterator.clone(), Vec::new())?;
        }
        Ok(())
    }

    // passes on an error after closing the iterator, keeping it over any error from closing
    fn fail_closing(&mut self, iterator: &Value, error: ScriptError) -> ScriptError {
        let thrown = self.take_thrown(error);
        if let Err(error) = self.close_iterator(iterator) {
            self.take_thrown(error);
        }
        self.throw_value(thrown)
    }

    fn call_closing(
        &mut self,
        iterator: &Value,
        callback: &Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        self.call_function(callback.clone(), Value::Undefined, arguments)
            .map_err(|error| self.fail_closing(iterator, error))
    }

    fn require_callable(&mut self, iterator: &Value, callback: &Value) -> Result<(), ScriptError> {
        if is_callable(callback) {
            return Ok(());
        }
        let error = self.type_error("Iterator helper requires a function");
        Err(self.fail_closing(iterator, error))
    }

    fn limit(&mut self, iterator: &Value, limit: Value) -> Result<f64, ScriptError> {
        let number = self
            .coerce_number(limit)
            .map_err(|error| self.fail_closing(iterator, error))?;
        if number.is_nan() || number.trunc() < 0.0 {
            let error = self.range_error(format!("{number} must be a positive number"));
            return Err(self.fail_closing(iterator, error));
        }
        Ok(number.trunc())
    }

    fn inherits_helpers(&self, value: &Value) -> bool {
        let Value::Object(object) = value else {
            return matches!(value, Value::Iterator(_));
        };
        let mut chain = object.borrow().explicit_prototype();
        while let Some(prototype) = chain {
            if ObjectHandle::ptr_eq(&prototype, &self.intrinsics.iterator_prototype) {
                return true;
            }
            chain = prototype.borrow().explicit_prototype();
        }
        false
    }

    fn helper_object(prototype: &ObjectHandle, iterator: Value, next: Value, step: Step) -> Value {
        let state = HelperState {
            iterator,
            next,
            step,
            counter: 0.0,
            done: false,
        };
        Value::Object(new_object(JsObject::with_prototype(
            Some(prototype.clone()),
            ObjectClass::IteratorHelper(Rc::new(RefCell::new(state))),
        )))
    }

    pub(super) fn construct_iterator(&mut self, new_target: Value) -> Result<Value, ScriptError> {
        let prototype = match &new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.iterator_prototype.clone(),
        };
        if ObjectHandle::ptr_eq(&prototype, &self.intrinsics.iterator_prototype) {
            return Err(self.type_error("Abstract class Iterator not directly constructable"));
        }
        Ok(Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Ordinary,
        ))))
    }

    pub(super) fn call_iterator_helper(
        &mut self,
        member: HelperMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        match member {
            HelperMember::Constructor => {
                return Err(self.type_error("Constructor Iterator requires 'new'"));
            }
            HelperMember::Next | HelperMember::Return => {
                let state = match &this {
                    Value::Object(object) => match &object.borrow().class {
                        ObjectClass::IteratorHelper(state) => Some(state.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(state) = state else {
                    return Err(self.type_error("Illegal invocation"));
                };
                return if member == HelperMember::Next {
                    self.helper_next(&state)
                } else {
                    self.helper_return(&state)
                };
            }
            HelperMember::From => return self.iterator_from(first.resolve()),
            _ => {}
        }
        if is_primitive(&this) {
            return Err(self.type_error("Iterator helper called on a value that is not an object"));
        }
        let iterator = this;
        let lazy = match member {
            HelperMember::Map | HelperMember::Filter | HelperMember::FlatMap => {
                self.require_callable(&iterator, &first)?;
                Some(match member {
                    HelperMember::Map => Step::Map(first.clone()),
                    HelperMember::Filter => Step::Filter(first.clone()),
                    _ => Step::FlatMap(first.clone(), None),
                })
            }
            HelperMember::Take => Some(Step::Take(self.limit(&iterator, first.clone())?)),
            HelperMember::Drop => Some(Step::Drop(self.limit(&iterator, first.clone())?)),
            HelperMember::ToArray => None,
            _ => {
                self.require_callable(&iterator, &first)?;
                None
            }
        };
        let next = self.get_property(&iterator, "next")?;
        if let Some(step) = lazy {
            let prototype = self.intrinsics.helpers.helper.clone();
            return Ok(Self::helper_object(&prototype, iterator, next, step));
        }
        let mut counter = 0.0;
        match member {
            HelperMember::ToArray => {
                let mut values = Vec::new();
                while let Some(value) = self.step_iterator(&iterator, &next)? {
                    values.push(value);
                }
                Ok(Self::new_array(values))
            }
            HelperMember::Reduce => {
                let mut total = match arguments.get(1) {
                    Some(initial) => initial.clone(),
                    None => match self.step_iterator(&iterator, &next)? {
                        Some(value) => {
                            counter = 1.0;
                            value
                        }
                        None => {
                            return Err(
                                self.type_error("Reduce of empty iterator with no initial value")
                            );
                        }
                    },
                };
                while let Some(value) = self.step_iterator(&iterator, &next)? {
                    let arguments = vec![total, value, Value::Number(counter)];
                    total = self.call_closing(&iterator, &first, arguments)?;
                    counter += 1.0;
                }
                Ok(total)
            }
            _ => {
                while let Some(value) = self.step_iterator(&iterator, &next)? {
                    let arguments = vec![value.clone(), Value::Number(counter)];
                    let answer = self.call_closing(&iterator, &first, arguments)?.truthy();
                    counter += 1.0;
                    let found = match member {
                        HelperMember::Some => answer.then_some(Value::Bool(true)),
                        HelperMember::Every => (!answer).then_some(Value::Bool(false)),
                        HelperMember::Find => answer.then_some(value),
                        _ => None,
                    };
                    if let Some(found) = found {
                        self.close_iterator(&iterator)?;
                        return Ok(found);
                    }
                }
                Ok(match member {
                    HelperMember::Some => Value::Bool(false),
                    HelperMember::Every => Value::Bool(true),
                    _ => Value::Undefined,
                })
            }
        }
    }

    fn iterator_from(&mut self, source: Value) -> Result<Value, ScriptError> {
        if !matches!(source, Value::String(_)) && is_primitive(&source) {
            return Err(self.type_error("Iterator.from requires an object or a string"));
        }
        let method = self.get_property(&source, "@@iterator")?;
        let iterator = if matches!(method, Value::Undefined | Value::Null) {
            source
        } else {
            let iterator = self.call_function(method, source, Vec::new())?;
            if is_primitive(&iterator) {
                return Err(
                    self.type_error("Result of the Symbol.iterator method is not an object")
                );
            }
            iterator
        };
        if self.inherits_helpers(&iterator) {
            return Ok(iterator);
        }
        let next = self.get_property(&iterator, "next")?;
        let wrap = self.intrinsics.helpers.wrap.clone();
        Ok(Self::helper_object(&wrap, iterator, next, Step::Wrap))
    }

    fn helper_next(&mut self, state: &Rc<RefCell<HelperState>>) -> Result<Value, ScriptError> {
        let result = self.advance_helper(state);
        if !matches!(result, Ok(Some(_))) {
            state.borrow_mut().done = true;
        }
        Ok(match result? {
            Some(value) => self.iterator_result(value, false),
            None => self.iterator_result(Value::Undefined, true),
        })
    }

    fn advance_helper(
        &mut self,
        state: &Rc<RefCell<HelperState>>,
    ) -> Result<Option<Value>, ScriptError> {
        if state.borrow().done {
            return Ok(None);
        }
        let (iterator, next) = {
            let state = state.borrow();
            (state.iterator.clone(), state.next.clone())
        };
        // the step is taken out while callbacks run, so a callback that calls next again
        // finds a plain pass through rather than a borrowed state
        let mut step = std::mem::replace(&mut state.borrow_mut().step, Step::Wrap);
        let result = self.advance_step(&iterator, &next, &mut step, state);
        state.borrow_mut().step = step;
        result
    }

    fn advance_step(
        &mut self,
        iterator: &Value,
        next: &Value,
        step: &mut Step,
        state: &Rc<RefCell<HelperState>>,
    ) -> Result<Option<Value>, ScriptError> {
        loop {
            if let Step::FlatMap(_, inner) = step
                && let Some((inner_iterator, inner_next)) = inner.clone()
            {
                match self.step_iterator(&inner_iterator, &inner_next) {
                    Ok(Some(value)) => return Ok(Some(value)),
                    Ok(None) => *inner = None,
                    Err(error) => return Err(self.fail_closing(iterator, error)),
                }
                continue;
            }
            match step {
                Step::Take(remaining) => {
                    if *remaining <= 0.0 {
                        self.close_iterator(iterator)?;
                        return Ok(None);
                    }
                    *remaining -= 1.0;
                }
                Step::Drop(remaining) => {
                    while *remaining > 0.0 {
                        *remaining -= 1.0;
                        if self.step_iterator(iterator, next)?.is_none() {
                            return Ok(None);
                        }
                    }
                }
                _ => {}
            }
            let Some(value) = self.step_iterator(iterator, next)? else {
                return Ok(None);
            };
            let counter = {
                let mut state = state.borrow_mut();
                state.counter += 1.0;
                Value::Number(state.counter - 1.0)
            };
            match step {
                Step::Map(callback) => {
                    let callback = callback.clone();
                    return self
                        .call_closing(iterator, &callback, vec![value, counter])
                        .map(Some);
                }
                Step::Filter(callback) => {
                    let callback = callback.clone();
                    let arguments = vec![value.clone(), counter];
                    if self.call_closing(iterator, &callback, arguments)?.truthy() {
                        return Ok(Some(value));
                    }
                }
                Step::FlatMap(callback, inner) => {
                    let callback = callback.clone();
                    let mapped = self.call_closing(iterator, &callback, vec![value, counter])?;
                    *inner = Some(self.flat_inner(iterator, mapped)?);
                }
                Step::Take(_) | Step::Drop(_) | Step::Wrap => return Ok(Some(value)),
            }
        }
    }

    // what a flatMap callback hands back must be an iterator or an iterable object; strings
    // are refused so they are not split into characters by accident
    fn flat_inner(
        &mut self,
        iterator: &Value,
        mapped: Value,
    ) -> Result<(Value, Value), ScriptError> {
        let opened = self.open_inner(mapped);
        opened.map_err(|error| self.fail_closing(iterator, error))
    }

    fn open_inner(&mut self, mapped: Value) -> Result<(Value, Value), ScriptError> {
        if is_primitive(&mapped) {
            return Err(self.type_error("flatMap callback must return an iterator or an iterable"));
        }
        let method = self.get_property(&mapped, "@@iterator")?;
        let inner = if matches!(method, Value::Undefined | Value::Null) {
            mapped
        } else {
            self.call_function(method, mapped, Vec::new())?
        };
        if is_primitive(&inner) {
            return Err(self.type_error("Result of the Symbol.iterator method is not an object"));
        }
        let next = self.get_property(&inner, "next")?;
        Ok((inner, next))
    }

    fn helper_return(&mut self, state: &Rc<RefCell<HelperState>>) -> Result<Value, ScriptError> {
        let (iterator, inner, done) = {
            let mut state = state.borrow_mut();
            let inner = match &mut state.step {
                Step::FlatMap(_, inner) => inner.take(),
                _ => None,
            };
            let done = std::mem::replace(&mut state.done, true);
            (state.iterator.clone(), inner, done)
        };
        if !done {
            if let Some((inner, _)) = inner {
                self.close_iterator(&inner)?;
            }
            self.close_iterator(&iterator)?;
        }
        Ok(self.iterator_result(Value::Undefined, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(source: &str) -> String {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(&format!(
                "document.getElementById('out').textContent = String((() => {{ {source} }})());"
            ))
            .unwrap_or_else(|error| panic!("{source}: {error:?}"));
        let out = document.borrow().find_element_by_id("out").unwrap();
        document
            .borrow()
            .element_text_content(out)
            .unwrap_or_default()
    }

    #[test]
    fn iterators_map_filter_take_drop_and_flat_map_lazily() {
        assert_eq!(
            run(
                "return [1, 2, 3, 4, 5].values().map((x) => x * 2).filter((x) => x > 2).drop(1).take(2).toArray().join();"
            ),
            "6,8"
        );
        assert_eq!(
            run("return [1, 2].values().flatMap((x) => [x, x * 10]).toArray().join();"),
            "1,10,2,20"
        );
        assert_eq!(
            run(
                "let read = 0; function* g() { while (true) { read++; yield read; } } g().map((x) => x).take(3).toArray(); return read;"
            ),
            "3"
        );
        assert_eq!(
            run("return new Map([[1, 'a'], [2, 'b']]).keys().reduce((sum, x) => sum + x, 0);"),
            "3"
        );
        assert_eq!(
            run(
                "return [[1, 2, 3].values().some((x) => x > 2), [1, 2].values().every((x) => x > 1), [5, 6].values().find((x) => x > 5)].join();"
            ),
            "true,false,6"
        );
        assert_eq!(
            run("return Object.prototype.toString.call([].values().map((x) => x));"),
            "[object Iterator Helper]"
        );
    }

    #[test]
    fn early_exits_close_the_iterator_and_bad_arguments_throw() {
        assert_eq!(
            run(
                "let closed = 0; const it = Object.setPrototypeOf({ next() { return { value: 1, done: false }; }, return() { closed++; return {}; } }, Iterator.prototype); it.find((x) => x === 1); it.take(0).next(); return closed;"
            ),
            "2"
        );
        assert_eq!(
            run("try { [].values().take(-1); return 'no'; } catch (error) { return error.name; }"),
            "RangeError"
        );
        assert_eq!(
            run("try { [].values().map(1); return 'no'; } catch (error) { return error.name; }"),
            "TypeError"
        );
        assert_eq!(
            run("try { new Iterator(); return 'no'; } catch (error) { return error.name; }"),
            "TypeError"
        );
        assert_eq!(
            run(
                "class Count extends Iterator { next() { return { done: true }; } } return new Count() instanceof Iterator;"
            ),
            "true"
        );
    }

    #[test]
    fn iterator_from_wraps_plain_iterators() {
        assert_eq!(
            run(
                "let n = 0; return Iterator.from({ next() { n++; return { value: n, done: n > 3 }; } }).map((x) => x * x).toArray().join();"
            ),
            "1,4,9"
        );
        assert_eq!(
            run("return Iterator.from('ab').toArray().join('-');"),
            "a-b"
        );
        assert_eq!(
            run("const it = [1].values(); return Iterator.from(it) === it;"),
            "true"
        );
    }
}
