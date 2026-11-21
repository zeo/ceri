// DisposableStack and AsyncDisposableStack: resources added with use, adopt and defer are
// disposed last first, and an error from a later disposal keeps the earlier one as its
// suppressed error, as the explicit resource management steps give

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, new_object};
use super::operations::{is_callable, is_primitive};
use super::promises::PromiseFunction;
use super::realm::{Builder, Native};
use super::*;
use std::cell::Cell;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum DisposableMember {
    Constructor,
    Disposed,
    Dispose,
    Use,
    Adopt,
    Defer,
    Move,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Call {
    // the method is called on the resource
    Method,
    // the method was handed to adopt and takes the resource as its argument
    Adopted,
    // an async stack fell back to Symbol.dispose, whose result is not awaited
    SyncFallback,
}

struct Resource {
    value: Value,
    method: Value,
    call: Call,
}

pub(super) struct DisposableStack {
    asynchronous: bool,
    disposed: Cell<bool>,
    resources: RefCell<Vec<Resource>>,
}

// an async disposal under way: what is left, the error so far, and whether anything was awaited
pub(super) struct Disposal {
    result: PromiseHandle,
    resources: RefCell<Vec<Resource>>,
    error: RefCell<Option<Value>>,
    awaited: Cell<bool>,
    needs_await: Cell<bool>,
}

pub(super) struct DisposableInterfaces {
    sync: ObjectHandle,
    asynchronous: ObjectHandle,
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
) -> (DisposableInterfaces, [(&'static str, Value); 2]) {
    let make = |asynchronous: bool| {
        let name = if asynchronous {
            "AsyncDisposableStack"
        } else {
            "DisposableStack"
        };
        let native = |member| Native::Disposable(member, asynchronous);
        let prototype = new_object(JsObject::with_prototype(
            Some(object_prototype.clone()),
            ObjectClass::Ordinary,
        ));
        let method = |key: &str, member, length| {
            let function = build.function(native(member), key, length);
            prototype.borrow_mut().define(
                key.to_string(),
                Property::hidden(Value::Object(function.clone())),
            );
            function
        };
        let (dispose_name, dispose_key) = if asynchronous {
            ("disposeAsync", "@@asyncDispose")
        } else {
            ("dispose", "@@dispose")
        };
        let dispose = method(dispose_name, DisposableMember::Dispose, 0);
        prototype.borrow_mut().define(
            dispose_key.to_string(),
            Property::hidden(Value::Object(dispose)),
        );
        method("use", DisposableMember::Use, 1);
        method("adopt", DisposableMember::Adopt, 2);
        method("defer", DisposableMember::Defer, 1);
        method("move", DisposableMember::Move, 0);
        build.getter(
            &prototype,
            "disposed",
            native(DisposableMember::Disposed),
            None,
        );
        build.value(&prototype, "@@toStringTag", Value::String(name.into()));
        let constructor =
            build.constructor(native(DisposableMember::Constructor), name, 0, &prototype);
        (prototype, (name, Value::Object(constructor)))
    };
    let (sync, sync_global) = make(false);
    let (asynchronous, async_global) = make(true);
    (
        DisposableInterfaces { sync, asynchronous },
        [sync_global, async_global],
    )
}

fn is_nullish(value: &Value) -> bool {
    matches!(value, Value::Undefined | Value::Null)
}

impl ScriptRuntime {
    fn disposable_stack(value: &Value, asynchronous: bool) -> Option<Rc<DisposableStack>> {
        let Value::Object(object) = value else {
            return None;
        };
        match &object.borrow().class {
            ObjectClass::DisposableStack(stack) if stack.asynchronous == asynchronous => {
                Some(stack.clone())
            }
            _ => None,
        }
    }

    fn disposable_object(&self, prototype: ObjectHandle, stack: DisposableStack) -> Value {
        Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::DisposableStack(Rc::new(stack)),
        )))
    }

    pub(super) fn construct_disposable(
        &mut self,
        asynchronous: bool,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let prototype = match &new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ if asynchronous => self.intrinsics.disposables.asynchronous.clone(),
            _ => self.intrinsics.disposables.sync.clone(),
        };
        Ok(self.disposable_object(
            prototype,
            DisposableStack {
                asynchronous,
                disposed: Cell::new(false),
                resources: RefCell::new(Vec::new()),
            },
        ))
    }

    // the resource a value stands for, or None for a nullish value a sync stack skips
    fn disposable_resource(
        &mut self,
        value: Value,
        asynchronous: bool,
    ) -> Result<Option<Resource>, ScriptError> {
        if is_nullish(&value) {
            // an async stack still owes an await for it when nothing else is awaited
            return Ok(asynchronous.then_some(Resource {
                value: Value::Undefined,
                method: Value::Undefined,
                call: Call::Method,
            }));
        }
        if is_primitive(&value) {
            return Err(self.type_error("Disposable resource must be an object"));
        }
        let mut call = Call::Method;
        let mut method = Value::Undefined;
        if asynchronous {
            method = self.get_property(&value, "@@asyncDispose")?;
        }
        if is_nullish(&method) {
            method = self.get_property(&value, "@@dispose")?;
            if asynchronous {
                call = Call::SyncFallback;
            }
        }
        if is_nullish(&method) {
            return Err(self.type_error(if asynchronous {
                "Symbol.asyncDispose is not defined on the resource"
            } else {
                "Symbol.dispose is not defined on the resource"
            }));
        }
        if !is_callable(&method) {
            return Err(self.type_error("Disposal method is not a function"));
        }
        Ok(Some(Resource {
            value,
            method,
            call,
        }))
    }

    fn call_resource(&mut self, resource: &Resource) -> Result<Value, ScriptError> {
        match resource.call {
            Call::Method => {
                self.call_function(resource.method.clone(), resource.value.clone(), Vec::new())
            }
            Call::Adopted => self.call_function(
                resource.method.clone(),
                Value::Undefined,
                vec![resource.value.clone()],
            ),
            Call::SyncFallback => self
                .call_function(resource.method.clone(), resource.value.clone(), Vec::new())
                .map(|_| Value::Undefined),
        }
    }

    // a later error wraps the one before it as its suppressed error
    fn suppress(&self, error: &mut Option<Value>, thrown: Value) {
        *error = Some(match error.take() {
            None => thrown,
            Some(previous) => {
                let wrapper =
                    self.make_error("SuppressedError", "An error was suppressed during disposal");
                if let Value::Object(object) = &wrapper {
                    let mut object = object.borrow_mut();
                    object.define("error".to_string(), Property::hidden(thrown));
                    object.define("suppressed".to_string(), Property::hidden(previous));
                }
                wrapper
            }
        });
    }

    fn dispose_resources(&mut self, resources: Vec<Resource>) -> Option<Value> {
        let mut error = None;
        for resource in resources.into_iter().rev() {
            if let Err(thrown) = self.call_resource(&resource) {
                let thrown = self.take_thrown(thrown);
                self.suppress(&mut error, thrown);
            }
        }
        error
    }

    fn dispose_resources_async(&mut self, resources: Vec<Resource>) -> PromiseHandle {
        let disposal = Rc::new(Disposal {
            result: Self::pending_promise(),
            resources: RefCell::new(resources),
            error: RefCell::new(None),
            awaited: Cell::new(false),
            needs_await: Cell::new(false),
        });
        self.continue_disposal(&disposal);
        disposal.result.clone()
    }

    fn continue_disposal(&mut self, disposal: &Rc<Disposal>) {
        loop {
            let next = disposal.resources.borrow_mut().pop();
            let Some(resource) = next else {
                if disposal.needs_await.get() && !disposal.awaited.get() {
                    disposal.awaited.set(true);
                    self.await_disposal(disposal, Value::Undefined);
                    return;
                }
                let outcome = match disposal.error.borrow_mut().take() {
                    Some(error) => PromiseOutcome::Rejected(error),
                    None => PromiseOutcome::Fulfilled(Value::Undefined),
                };
                self.settle_promise(&disposal.result, outcome);
                return;
            };
            if is_nullish(&resource.method) {
                disposal.needs_await.set(true);
                continue;
            }
            match self.call_resource(&resource) {
                Ok(value) => {
                    disposal.awaited.set(true);
                    self.await_disposal(disposal, value);
                    return;
                }
                Err(thrown) => {
                    let thrown = self.take_thrown(thrown);
                    if resource.call == Call::SyncFallback {
                        // the fallback hands back a rejected promise, which is still awaited
                        disposal.awaited.set(true);
                        let rejected = new_promise(PromiseOutcome::Rejected(thrown));
                        self.await_disposal(disposal, Value::Promise(rejected));
                        return;
                    }
                    self.suppress(&mut disposal.error.borrow_mut(), thrown);
                }
            }
        }
    }

    fn await_disposal(&mut self, disposal: &Rc<Disposal>, value: Value) {
        let step = |rejected| {
            Value::Builtin(Builtin::Native(Native::Promise(Rc::new(
                PromiseFunction::DisposalStep(disposal.clone(), rejected),
            ))))
        };
        let waited = self
            .promise_resolve(value)
            .and_then(|promise| self.promise_then(promise, Some(&step(false)), Some(&step(true))));
        if let Err(thrown) = waited {
            let thrown = self.take_thrown(thrown);
            self.suppress(&mut disposal.error.borrow_mut(), thrown);
            self.continue_disposal(disposal);
        }
    }

    pub(super) fn step_disposal(
        &mut self,
        disposal: &Rc<Disposal>,
        rejected: bool,
        settled: Value,
    ) -> Result<Value, ScriptError> {
        if rejected {
            self.suppress(&mut disposal.error.borrow_mut(), settled);
        }
        self.continue_disposal(disposal);
        Ok(Value::Undefined)
    }

    pub(super) fn call_disposable(
        &mut self,
        member: DisposableMember,
        asynchronous: bool,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        let name = if asynchronous {
            "AsyncDisposableStack"
        } else {
            "DisposableStack"
        };
        if member == DisposableMember::Constructor {
            return Err(self.type_error(format!("Constructor {name} requires 'new'")));
        }
        let Some(stack) = Self::disposable_stack(&this, asynchronous) else {
            if asynchronous && member == DisposableMember::Dispose {
                return Ok(Value::Promise(self.rejected_promise(format!(
                    "{name}.prototype.disposeAsync called on an incompatible receiver"
                ))));
            }
            return Err(self.type_error(format!(
                "Method {name}.prototype.{} called on an incompatible receiver",
                match member {
                    DisposableMember::Disposed => "disposed",
                    DisposableMember::Dispose => "dispose",
                    DisposableMember::Use => "use",
                    DisposableMember::Adopt => "adopt",
                    DisposableMember::Defer => "defer",
                    _ => "move",
                }
            )));
        };
        match member {
            DisposableMember::Disposed => return Ok(Value::Bool(stack.disposed.get())),
            DisposableMember::Dispose => {
                let resources = if stack.disposed.replace(true) {
                    Vec::new()
                } else {
                    std::mem::take(&mut *stack.resources.borrow_mut())
                };
                if asynchronous {
                    return Ok(Value::Promise(self.dispose_resources_async(resources)));
                }
                return match self.dispose_resources(resources) {
                    Some(error) => Err(self.throw_value(error)),
                    None => Ok(Value::Undefined),
                };
            }
            _ => {}
        }
        if stack.disposed.get() {
            return Err(self.reference_error(format!("Cannot use a disposed {name}")));
        }
        match member {
            DisposableMember::Use => {
                let value = argument(0);
                if let Some(resource) = self.disposable_resource(value.clone(), asynchronous)? {
                    stack.resources.borrow_mut().push(resource);
                }
                Ok(value)
            }
            DisposableMember::Adopt => {
                let (value, on_dispose) = (argument(0), argument(1));
                if !is_callable(&on_dispose) {
                    return Err(self.type_error("onDispose is not a function"));
                }
                stack.resources.borrow_mut().push(Resource {
                    value: value.clone(),
                    method: on_dispose,
                    call: Call::Adopted,
                });
                Ok(value)
            }
            DisposableMember::Defer => {
                let on_dispose = argument(0);
                if !is_callable(&on_dispose) {
                    return Err(self.type_error("onDispose is not a function"));
                }
                stack.resources.borrow_mut().push(Resource {
                    value: Value::Undefined,
                    method: on_dispose,
                    call: Call::Method,
                });
                Ok(Value::Undefined)
            }
            _ => {
                let resources = std::mem::take(&mut *stack.resources.borrow_mut());
                stack.disposed.set(true);
                let prototype = if asynchronous {
                    self.intrinsics.disposables.asynchronous.clone()
                } else {
                    self.intrinsics.disposables.sync.clone()
                };
                Ok(self.disposable_object(
                    prototype,
                    DisposableStack {
                        asynchronous,
                        disposed: Cell::new(false),
                        resources: RefCell::new(resources),
                    },
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacks_dispose_last_first_and_chain_suppressed_errors() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                const out = document.getElementById("out");
                const stack = new DisposableStack();
                const resource = { [Symbol.dispose]() { seen.push("used:" + (this === resource)); } };
                seen.push(stack.use(resource) === resource, stack.use(null));
                stack.adopt(7, (value) => seen.push("adopted:" + value));
                stack.defer(function () { "use strict"; seen.push("deferred:" + (this === undefined)); });
                const moved = stack.move();
                seen.push(stack.disposed, moved.disposed);
                try { stack.use(resource); } catch (error) { seen.push(error.name); }
                moved[Symbol.dispose]();
                seen.push(moved.disposed, moved.dispose());
                const failing = new DisposableStack();
                failing.defer(() => { throw new Error("first"); });
                failing.defer(() => { throw new Error("second"); });
                try { failing.dispose(); } catch (error) {
                    seen.push(error.name, error.error.message, error.suppressed.message);
                }
                try { new DisposableStack().use(1); } catch (error) { seen.push(error.name); }
                try { DisposableStack(); } catch (error) { seen.push(error.name); }
                const wrapped = new SuppressedError(1, 2, "m");
                seen.push(wrapped.error, wrapped.suppressed, wrapped.message, SuppressedError.length);
                const aggregate = new AggregateError(new Set(["a"]), "all", { cause: "c" });
                seen.push(aggregate.message, aggregate.errors.join(), aggregate.cause);
                seen.push(String(Symbol.dispose), Object.prototype.toString.call(stack));
                (async () => {
                    const later = new AsyncDisposableStack();
                    later.use({ [Symbol.dispose]() { seen.push("sync fallback"); return Promise.reject(new Error("ignored")); } });
                    later.use({ async [Symbol.asyncDispose]() { await null; seen.push("async"); } });
                    later.use(undefined);
                    await later.disposeAsync();
                    seen.push(later.disposed);
                    const rejecting = new AsyncDisposableStack();
                    rejecting.defer(async () => { throw new RangeError("late"); });
                    try { await rejecting[Symbol.asyncDispose](); } catch (error) { seen.push(error.name); }
                    await AsyncDisposableStack.prototype.disposeAsync.call({}).catch((error) => seen.push(error.name));
                    out.textContent = seen.join("|");
                })();
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(
                "true||true|false|ReferenceError|deferred:true|adopted:7|used:true|true||\
                 SuppressedError|first|second|TypeError|TypeError|1|2|m|3|all|a|c|\
                 Symbol(Symbol.dispose)|[object DisposableStack]|async|sync fallback|true|\
                 RangeError|TypeError"
            )
        );
    }
}
