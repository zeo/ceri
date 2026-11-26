// the Promise constructor, its prototype methods and statics (resolve, reject, all,
// allSettled, race, any, withResolvers and try), built on the runtime's reaction queue with
// small native functions that carry their shared state; a constructor other than Promise,
// such as a subclass, is reached through NewPromiseCapability as the standard says

use super::operations::{is_callable, is_primitive, same_value};
use super::*;
use std::cell::Cell;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Combinator {
    All,
    AllSettled,
    AllKeyed,
    AllSettledKeyed,
    Any,
    Race,
}

#[derive(Default)]
pub(super) struct Capability {
    resolve: RefCell<Option<Value>>,
    reject: RefCell<Option<Value>>,
}

pub(super) struct Aggregate {
    keys: RefCell<Vec<String>>,
    values: RefCell<Vec<Value>>,
    remaining: Cell<usize>,
    resolve: Value,
    reject: Value,
    combinator: Combinator,
}

#[derive(Clone)]
pub(super) enum PromiseFunction {
    Combinator(Combinator),
    WithResolvers,
    // calls a function now and hands back a promise of what it returns or throws
    Try,
    Constructor,
    Then,
    Catch,
    Finally,
    Resolve,
    Reject,
    // the Symbol.species getter, which hands back its receiver
    Species,
    // the executor a promise subclass is constructed with, catching its resolve and reject
    Capability(Rc<Capability>),
    // the resolve or reject function of a promise; the pair share whether one already ran
    Resolving {
        promise: PromiseHandle,
        done: Rc<Cell<bool>>,
        reject: bool,
    },
    // a per-element reaction of a combinator, which only counts its first call
    Element {
        state: Rc<Aggregate>,
        index: usize,
        rejected: bool,
        called: Rc<Cell<bool>>,
    },
    // the reaction finally installs: run the callback, then pass the outcome through
    AfterFinally {
        callback: Value,
        constructor: Value,
        rejected: bool,
    },
    // resolves to the saved value, or rethrows the saved reason
    Settle {
        value: Value,
        rejected: bool,
    },
    FromAsync,
    // a step of Array.fromAsync once what it awaited settled
    FromAsyncStep(Rc<super::from_async::FromAsync>, super::from_async::Phase),
    DisposalStep(Rc<super::disposables::AsyncDisposal>, bool),
}

impl PromiseFunction {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        match self {
            Self::Capability(cap) => {
                if tracer.visit_rc(cap) {
                    if let Some(v) = &*cap.resolve.borrow() {
                        tracer.trace_value(v);
                    }
                    if let Some(v) = &*cap.reject.borrow() {
                        tracer.trace_value(v);
                    }
                }
            }
            Self::Resolving { promise, .. } => tracer.trace_promise(promise),
            Self::Element { state, .. } => {
                if tracer.visit_rc(state) {
                    for value in state.values.borrow().iter() {
                        tracer.trace_value(value);
                    }
                    tracer.trace_value(&state.resolve);
                    tracer.trace_value(&state.reject);
                }
            }
            Self::AfterFinally {
                callback,
                constructor,
                ..
            } => {
                tracer.trace_value(callback);
                tracer.trace_value(constructor);
            }
            Self::Settle { value, .. } => tracer.trace_value(value),
            Self::FromAsyncStep(state, _) => state.trace(tracer),
            Self::DisposalStep(disposal, _) => disposal.trace(tracer),
            _ => {}
        }
    }
}

fn argument(arguments: &[Value], index: usize) -> Value {
    arguments.get(index).cloned().unwrap_or(Value::Undefined)
}

impl ScriptRuntime {
    // an anonymous built-in function object, as the standard's abstract closures are
    fn promise_closure(&self, function: PromiseFunction, length: u32) -> Value {
        self.native_function(Native::Promise(Rc::new(function)), "", length)
    }

    pub(super) fn resolving_functions(&self, promise: &PromiseHandle) -> (Value, Value) {
        let done = Rc::new(Cell::new(false));
        let make = |reject| {
            self.promise_closure(
                PromiseFunction::Resolving {
                    promise: promise.clone(),
                    done: done.clone(),
                    reject,
                },
                1,
            )
        };
        (make(false), make(true))
    }

    fn settle_aggregate(&mut self, state: &Aggregate) -> Result<(), ScriptError> {
        if state.remaining.get() != 0 {
            return Ok(());
        }
        if matches!(
            state.combinator,
            Combinator::AllKeyed | Combinator::AllSettledKeyed
        ) {
            let keys = std::mem::take(&mut *state.keys.borrow_mut());
            let values = std::mem::take(&mut *state.values.borrow_mut());
            let object = object::new_object(object::JsObject::with_prototype(
                None,
                object::ObjectClass::Ordinary,
            ));
            {
                let mut borrowed = object.borrow_mut();
                for (key, value) in keys.into_iter().zip(values) {
                    borrowed.define(key, object::Property::data(value));
                }
            }
            self.call_function(
                state.resolve.clone(),
                Value::Undefined,
                vec![Value::Object(object)],
            )?;
            return Ok(());
        }
        let values = Self::new_array(std::mem::take(&mut *state.values.borrow_mut()));
        if state.combinator == Combinator::Any {
            let error = self.make_error("AggregateError", "All promises were rejected");
            if let Value::Object(object) = &error {
                object
                    .borrow_mut()
                    .define("errors".to_string(), object::Property::hidden(values));
            }
            self.call_function(state.reject.clone(), Value::Undefined, vec![error])?;
        } else {
            self.call_function(state.resolve.clone(), Value::Undefined, vec![values])?;
        }
        Ok(())
    }

    pub(super) fn is_intrinsic_promise(&self, value: &Value) -> bool {
        matches!(value, Value::Object(object) if ObjectHandle::ptr_eq(object, &self.intrinsics.promise_constructor))
    }

    // NewPromiseCapability: a promise made by a constructor with the resolve and reject it
    // handed its executor; the intrinsic Promise needs no executor round trip
    pub(super) fn promise_capability(
        &mut self,
        constructor: &Value,
    ) -> Result<(Value, Value, Value), ScriptError> {
        if self.is_intrinsic_promise(constructor) {
            let promise = Self::pending_promise();
            let (resolve, reject) = self.resolving_functions(&promise);
            return Ok((Value::Promise(promise), resolve, reject));
        }
        if !Self::is_constructor(constructor) {
            return Err(self.type_error("Promise capability requires a constructor"));
        }
        let slots = Rc::new(Capability::default());
        let executor = self.promise_closure(PromiseFunction::Capability(slots.clone()), 2);
        let promise = self.construct(constructor.clone(), vec![executor], None)?;
        let resolve = slots.resolve.borrow().clone().unwrap_or(Value::Undefined);
        let reject = slots.reject.borrow().clone().unwrap_or(Value::Undefined);
        if !is_callable(&resolve) || !is_callable(&reject) {
            return Err(self.type_error("Promise resolve or reject function is not callable"));
        }
        Ok((promise, resolve, reject))
    }

    // PromiseResolve(C, x): a promise made by C already passes through
    fn promise_resolve_with(
        &mut self,
        constructor: &Value,
        value: Value,
    ) -> Result<Value, ScriptError> {
        if let Value::Promise(_) = &value {
            let made_by = self.get_property(&value, "constructor")?;
            if same_value(&made_by, constructor) {
                return Ok(value);
            }
        }
        let (promise, resolve, _) = self.promise_capability(constructor)?;
        self.call_function(resolve, Value::Undefined, vec![value])?;
        Ok(promise)
    }

    fn invoke_then(&mut self, promise: Value, arguments: Vec<Value>) -> Result<Value, ScriptError> {
        let then = self.get_property(&promise, "then")?;
        self.call_function(then, promise, arguments)
    }

    // SpeciesConstructor(promise, %Promise%)
    fn promise_species(&mut self, promise: &Value) -> Result<Value, ScriptError> {
        let intrinsic = Value::Object(self.intrinsics.promise_constructor.clone());
        let constructor = self.get_property(promise, "constructor")?;
        if matches!(constructor, Value::Undefined) {
            return Ok(intrinsic);
        }
        if is_primitive(&constructor) {
            return Err(self.type_error("The promise constructor is not an object"));
        }
        let species = self.get_property(&constructor, "@@species")?;
        if matches!(species, Value::Undefined | Value::Null) {
            return Ok(intrinsic);
        }
        if !Self::is_constructor(&species) {
            return Err(self.type_error("Promise species is not a constructor"));
        }
        Ok(species)
    }

    pub(super) fn construct_promise_object(
        &mut self,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let executor = argument(&arguments, 0);
        if !is_callable(&executor) {
            return Err(self.type_error("Promise resolver is not a function"));
        }
        let promise = Self::pending_promise();
        if matches!(new_target, Value::Object(_)) && !self.is_intrinsic_promise(&new_target) {
            let prototype = match self.get_property(&new_target, "prototype")? {
                Value::Object(prototype) => prototype,
                _ => self.intrinsics.promise_prototype.clone(),
            };
            super::promise_object(&promise)
                .borrow_mut()
                .set_prototype(Some(prototype));
        }
        let (resolve, reject) = self.resolving_functions(&promise);
        if let Err(error) =
            self.call_function(executor, Value::Undefined, vec![resolve, reject.clone()])
        {
            let reason = self.take_thrown(error);
            self.call_function(reject, Value::Undefined, vec![reason])?;
        }
        Ok(Value::Promise(promise))
    }

    // the loop of all, allSettled, any and race over an iterable
    fn run_combinator(
        &mut self,
        combinator: Combinator,
        constructor: &Value,
        iterable: Value,
        resolve: &Value,
        reject: &Value,
    ) -> Result<(), ScriptError> {
        let promise_resolve = self.get_property(constructor, "resolve")?;
        if !is_callable(&promise_resolve) {
            return Err(self.type_error("Promise resolve is not a function"));
        }
        let iterator = self.get_iterator(iterable)?;
        let state = Rc::new(Aggregate {
            keys: RefCell::new(Vec::new()),
            values: RefCell::new(Vec::new()),
            remaining: Cell::new(1),
            resolve: resolve.clone(),
            reject: reject.clone(),
            combinator,
        });
        let mut count = 0;
        while let Some(item) = self.iterator_step(&iterator)? {
            count += 1;
            let step = if count > MAX_USER_ITERATOR_VALUES {
                Err(self.range_error("Promise combinator input exceeded the deterministic limit"))
            } else {
                self.combine_item(&state, constructor, &promise_resolve, item)
            };
            if let Err(error) = step {
                let thrown = self.take_thrown(error);
                if let Err(error) = self.iterator_close(&iterator) {
                    self.take_thrown(error);
                }
                return Err(self.throw_value(thrown));
            }
        }
        if combinator != Combinator::Race {
            state.remaining.set(state.remaining.get() - 1);
            self.settle_aggregate(&state)?;
        }
        Ok(())
    }

    fn run_keyed_combinator(
        &mut self,
        combinator: Combinator,
        constructor: &Value,
        promises: Value,
        resolve: &Value,
        reject: &Value,
    ) -> Result<(), ScriptError> {
        let promise_resolve = self.get_property(constructor, "resolve")?;
        if !is_callable(&promise_resolve) {
            return Err(self.type_error("Promise resolve is not a function"));
        }
        if is_primitive(&promises) {
            return Err(self.type_error("Promise keyed combinator requires an object"));
        }
        let all_keys = self.all_own_property_keys(&promises)?;
        let state = Rc::new(Aggregate {
            keys: RefCell::new(Vec::new()),
            values: RefCell::new(Vec::new()),
            remaining: Cell::new(1),
            resolve: resolve.clone(),
            reject: reject.clone(),
            combinator,
        });
        for key in all_keys {
            let Some(property) = self.own_property_of(&promises, &key)? else {
                continue;
            };
            if !property.enumerable {
                continue;
            }
            let item = self.get_property(&promises, &key)?;
            let next =
                self.call_function(promise_resolve.clone(), constructor.clone(), vec![item])?;
            let index = {
                state.keys.borrow_mut().push(key);
                let mut values = state.values.borrow_mut();
                values.push(Value::Undefined);
                values.len() - 1
            };
            let called = Rc::new(Cell::new(false));
            let element = |rejected| {
                self.promise_closure(
                    PromiseFunction::Element {
                        state: state.clone(),
                        index,
                        rejected,
                        called: called.clone(),
                    },
                    1,
                )
            };
            let (fulfilled, rejected) = match combinator {
                Combinator::AllKeyed => (element(false), reject.clone()),
                Combinator::AllSettledKeyed => (element(false), element(true)),
                _ => unreachable!("keyed combinator only"),
            };
            state.remaining.set(state.remaining.get() + 1);
            self.invoke_then(next, vec![fulfilled, rejected])?;
        }
        state.remaining.set(state.remaining.get() - 1);
        self.settle_aggregate(&state)?;
        Ok(())
    }

    fn combine_item(
        &mut self,
        state: &Rc<Aggregate>,
        constructor: &Value,
        promise_resolve: &Value,
        item: Value,
    ) -> Result<(), ScriptError> {
        let next = self.call_function(promise_resolve.clone(), constructor.clone(), vec![item])?;
        let (resolve, reject) = (state.resolve.clone(), state.reject.clone());
        if state.combinator == Combinator::Race {
            return self.invoke_then(next, vec![resolve, reject]).map(|_| ());
        }
        let index = {
            let mut values = state.values.borrow_mut();
            values.push(Value::Undefined);
            values.len() - 1
        };
        let called = Rc::new(Cell::new(false));
        let element = |rejected| {
            self.promise_closure(
                PromiseFunction::Element {
                    state: state.clone(),
                    index,
                    rejected,
                    called: called.clone(),
                },
                1,
            )
        };
        let (fulfilled, rejected) = match state.combinator {
            Combinator::All => (element(false), reject),
            Combinator::AllSettled => (element(false), element(true)),
            _ => (resolve, element(true)),
        };
        state.remaining.set(state.remaining.get() + 1);
        self.invoke_then(next, vec![fulfilled, rejected])
            .map(|_| ())
    }

    pub(super) fn call_promise_function(
        &mut self,
        function: PromiseFunction,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        match function {
            PromiseFunction::Constructor => {
                Err(self.type_error("Promise constructor cannot be invoked without 'new'"))
            }
            PromiseFunction::Species => Ok(this),
            PromiseFunction::Capability(slots) => {
                let taken = |slot: &RefCell<Option<Value>>| {
                    !matches!(*slot.borrow(), None | Some(Value::Undefined))
                };
                if taken(&slots.resolve) || taken(&slots.reject) {
                    return Err(self.type_error("Promise executor has already been invoked"));
                }
                *slots.resolve.borrow_mut() = Some(argument(&arguments, 0));
                *slots.reject.borrow_mut() = Some(argument(&arguments, 1));
                Ok(Value::Undefined)
            }
            PromiseFunction::Resolving {
                promise,
                done,
                reject,
            } => {
                if done.replace(true) {
                    return Ok(Value::Undefined);
                }
                let value = argument(&arguments, 0);
                if reject {
                    self.settle_promise(&promise, PromiseOutcome::Rejected(value));
                } else {
                    self.resolve_promise(&promise, value)?;
                }
                Ok(Value::Undefined)
            }
            PromiseFunction::Then => {
                let Value::Promise(promise) = this.resolve() else {
                    return Err(self.type_error(
                        "Promise.prototype.then called on a value that is not a promise",
                    ));
                };
                let constructor = self.promise_species(&this)?;
                let (fulfilled, rejected) = (argument(&arguments, 0), argument(&arguments, 1));
                if self.is_intrinsic_promise(&constructor) {
                    return self.promise_then(promise, Some(&fulfilled), Some(&rejected));
                }
                let (result, resolve, reject) = self.promise_capability(&constructor)?;
                let Value::Promise(derived) =
                    self.promise_then(promise, Some(&fulfilled), Some(&rejected))?
                else {
                    unreachable!("promise_then hands back a promise")
                };
                self.promise_then(derived, Some(&resolve), Some(&reject))?;
                Ok(result)
            }
            PromiseFunction::Catch => {
                self.invoke_then(this, vec![Value::Undefined, argument(&arguments, 0)])
            }
            PromiseFunction::Finally => {
                if is_primitive(&this) {
                    return Err(self.type_error(
                        "Promise.prototype.finally called on a value that is not an object",
                    ));
                }
                let constructor = self.promise_species(&this)?;
                let callback = argument(&arguments, 0);
                if !is_callable(&callback) {
                    return self.invoke_then(this, vec![callback.clone(), callback]);
                }
                let after = |rejected| {
                    self.promise_closure(
                        PromiseFunction::AfterFinally {
                            callback: callback.clone(),
                            constructor: constructor.clone(),
                            rejected,
                        },
                        1,
                    )
                };
                let (fulfilled, rejected) = (after(false), after(true));
                self.invoke_then(this, vec![fulfilled, rejected])
            }
            PromiseFunction::Resolve => {
                if is_primitive(&this) {
                    return Err(
                        self.type_error("Promise.resolve called on a value that is not an object")
                    );
                }
                self.promise_resolve_with(&this, argument(&arguments, 0))
            }
            PromiseFunction::Reject => {
                if is_primitive(&this) {
                    return Err(
                        self.type_error("Promise.reject called on a value that is not an object")
                    );
                }
                let (promise, _, reject) = self.promise_capability(&this)?;
                self.call_function(reject, Value::Undefined, vec![argument(&arguments, 0)])?;
                Ok(promise)
            }
            PromiseFunction::Combinator(combinator) => {
                if is_primitive(&this) {
                    return Err(self
                        .type_error("Promise combinator called on a value that is not an object"));
                }
                let (promise, resolve, reject) = self.promise_capability(&this)?;
                let input = argument(&arguments, 0);
                let run = if matches!(
                    combinator,
                    Combinator::AllKeyed | Combinator::AllSettledKeyed
                ) {
                    self.run_keyed_combinator(combinator, &this, input, &resolve, &reject)
                } else {
                    self.run_combinator(combinator, &this, input, &resolve, &reject)
                };
                if let Err(error) = run {
                    let reason = self.take_thrown(error);
                    self.call_function(reject, Value::Undefined, vec![reason])?;
                }
                Ok(promise)
            }
            PromiseFunction::Element {
                state,
                index,
                rejected,
                called,
            } => {
                if called.replace(true) {
                    return Ok(Value::Undefined);
                }
                let value = argument(&arguments, 0);
                let stored = match state.combinator {
                    Combinator::AllSettled | Combinator::AllSettledKeyed => {
                        let (status, key) = if rejected {
                            ("rejected", "reason")
                        } else {
                            ("fulfilled", "value")
                        };
                        self.object_from_entries(vec![
                            ("status".to_string(), Value::String(status.into())),
                            (key.to_string(), value),
                        ])
                    }
                    _ => value,
                };
                if let Some(slot) = state.values.borrow_mut().get_mut(index) {
                    *slot = stored;
                }
                state.remaining.set(state.remaining.get().saturating_sub(1));
                self.settle_aggregate(&state)?;
                Ok(Value::Undefined)
            }
            PromiseFunction::Try => {
                if is_primitive(&this) {
                    return Err(
                        self.type_error("Promise.try called on a value that is not an object")
                    );
                }
                let mut arguments = arguments.into_iter();
                let callback = arguments.next().unwrap_or(Value::Undefined);
                // what the callback returns goes through PromiseResolve, so a promise of this
                // constructor comes back unwrapped
                match self.call_function(callback, Value::Undefined, arguments.collect()) {
                    Ok(value) => self.promise_resolve_with(&this, value),
                    Err(error) => {
                        let thrown = self.take_thrown(error);
                        let (promise, _, reject) = self.promise_capability(&this)?;
                        self.call_function(reject, Value::Undefined, vec![thrown])?;
                        Ok(promise)
                    }
                }
            }
            PromiseFunction::WithResolvers => {
                let (promise, resolve, reject) = self.promise_capability(&this)?;
                Ok(self.object_from_entries(vec![
                    ("promise".to_string(), promise),
                    ("resolve".to_string(), resolve),
                    ("reject".to_string(), reject),
                ]))
            }
            PromiseFunction::AfterFinally {
                callback,
                constructor,
                rejected,
            } => {
                let outcome = argument(&arguments, 0);
                let result = self.call_function(callback, Value::Undefined, Vec::new())?;
                let waited = self.promise_resolve_with(&constructor, result)?;
                let settle = self.promise_closure(
                    PromiseFunction::Settle {
                        value: outcome,
                        rejected,
                    },
                    0,
                );
                self.invoke_then(waited, vec![settle])
            }
            PromiseFunction::Settle { value, rejected } => {
                if rejected {
                    return Err(self.throw_value(value));
                }
                Ok(value)
            }
            PromiseFunction::FromAsync => Ok(self.array_from_async(this, arguments)),
            PromiseFunction::FromAsyncStep(state, phase) => {
                self.step_from_async(&state, phase, argument(&arguments, 0))
            }
            PromiseFunction::DisposalStep(disposal, rejected) => {
                self.step_disposal(&disposal, rejected, argument(&arguments, 0))
            }
        }
    }

    pub(super) fn object_from_entries(&self, entries: Vec<(String, Value)>) -> Value {
        let object = self.new_plain_object();
        {
            let mut object = object.borrow_mut();
            for (key, value) in entries {
                object.insert(key, value);
            }
        }
        Value::Object(object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn promises_are_objects_that_subclasses_and_patches_reach() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                class Tracked extends Promise {
                    constructor(executor) { super(executor); this.label = "tracked"; }
                }
                const tracked = new Tracked((resolve) => resolve(1));
                const derived = tracked.then((value) => value + 1);
                seen.push(tracked instanceof Tracked, derived instanceof Tracked, tracked.label);
                const plain = Promise.resolve(2);
                plain.note = "kept";
                seen.push(plain.note, Object.getPrototypeOf(plain) === Promise.prototype, typeof Promise.prototype.then);
                seen.push(Promise.resolve(plain) === plain, Tracked.resolve(plain) instanceof Tracked);
                const original = Promise.prototype.then;
                let patched = 0;
                Promise.prototype.then = function (...args) { patched++; return original.apply(this, args); };
                Promise.all([plain]).then(() => {
                    Promise.prototype.then = original;
                    seen.push(patched > 0);
                    derived.then((value) => {
                        seen.push(value);
                        document.getElementById("out").textContent = seen.join("|");
                    });
                });
                try { Promise(() => {}); } catch (error) { seen.push(error.name); }
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some("true|true|tracked|kept|true|function|true|true|TypeError|true|2")
        );
    }

    #[test]
    fn promise_keyed_combinators_resolve_null_proto_objects_and_settle_rejections() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                (async () => {
                    const sym = Symbol("k");
                    const input = { a: Promise.resolve(1), b: 2, [sym]: Promise.resolve(3) };
                    Object.defineProperty(input, "hidden", { value: Promise.reject("skip"), enumerable: false });
                    const all = await Promise.allKeyed(input);
                    seen.push(Object.getPrototypeOf(all), all.a, all.b, all[sym], "hidden" in all);
                    const empty = await Promise.allKeyed({});
                    seen.push(Object.getPrototypeOf(empty), Object.keys(empty).length);
                    try {
                        await Promise.allKeyed({ ok: Promise.resolve(1), bad: Promise.reject("boom") });
                    } catch (err) {
                        seen.push(err);
                    }
                    const settled = await Promise.allSettledKeyed({ x: Promise.resolve(9), y: Promise.reject("no") });
                    seen.push(Object.getPrototypeOf(settled), settled.x.status, settled.x.value, settled.y.status, settled.y.reason);
                    document.getElementById("out").textContent = seen.join("|");
                })();
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some("|1|2|3|false||0|boom||fulfilled|9|rejected|no")
        );
    }
}
