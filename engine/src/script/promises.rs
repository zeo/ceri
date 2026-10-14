// the Promise combinators (all, allSettled, race, any), withResolvers and finally, built on
// the runtime's reaction queue with small native callbacks that carry their shared state

use super::operations::is_callable;
use super::*;
use std::cell::Cell;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Combinator {
    All,
    AllSettled,
    Any,
}

pub(super) struct Aggregate {
    values: RefCell<Vec<Value>>,
    remaining: Cell<usize>,
    promise: PromiseHandle,
    combinator: Combinator,
}

#[derive(Clone)]
pub(super) enum PromiseFunction {
    All,
    AllSettled,
    Race,
    Any,
    WithResolvers,
    Finally(PromiseHandle),
    // a per-element reaction of a combinator
    Element {
        state: Rc<Aggregate>,
        index: usize,
        rejected: bool,
    },
    // the reaction finally installs: run the callback, then pass the outcome through
    AfterFinally {
        callback: Value,
        rejected: bool,
    },
    // resolves to the saved value, or rethrows the saved reason
    Settle {
        value: Value,
        rejected: bool,
    },
}

fn argument(arguments: &[Value], index: usize) -> Value {
    arguments.get(index).cloned().unwrap_or(Value::Undefined)
}

impl ScriptRuntime {
    fn native_value(function: PromiseFunction) -> Value {
        Value::Builtin(Builtin::Native(Native::Promise(Rc::new(function))))
    }

    fn settle_aggregate(&mut self, state: &Aggregate) {
        if state.remaining.get() != 0 {
            return;
        }
        let values = std::mem::take(&mut *state.values.borrow_mut());
        if state.combinator == Combinator::Any {
            let error = self.make_error("AggregateError", "All promises were rejected");
            if let Value::Object(object) = &error {
                object.borrow_mut().define(
                    "errors".to_string(),
                    object::Property::hidden(Self::new_array(values)),
                );
            }
            self.settle_promise(&state.promise, PromiseOutcome::Rejected(error));
        } else {
            self.settle_promise(
                &state.promise,
                PromiseOutcome::Fulfilled(Self::new_array(values)),
            );
        }
    }

    pub(super) fn call_promise_function(
        &mut self,
        function: PromiseFunction,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        match function {
            PromiseFunction::All
            | PromiseFunction::AllSettled
            | PromiseFunction::Any
            | PromiseFunction::Race => {
                let result = Self::pending_promise();
                let items = match self.iterate_to_vec(argument(&arguments, 0)) {
                    Ok(items) => items,
                    Err(error) => {
                        let reason = self.take_thrown(error);
                        return Ok(Value::Promise(self.rejected_promise_value(reason)));
                    }
                };
                if matches!(function, PromiseFunction::Race) {
                    for item in items {
                        let promise = self.promise_resolve(item)?;
                        let resolve =
                            Value::Builtin(Builtin::PromiseResolvePending(result.clone()));
                        let reject = Value::Builtin(Builtin::PromiseRejectPending(result.clone()));
                        self.promise_then(promise, Some(&resolve), Some(&reject))?;
                    }
                    return Ok(Value::Promise(result));
                }
                let combinator = match function {
                    PromiseFunction::All => Combinator::All,
                    PromiseFunction::AllSettled => Combinator::AllSettled,
                    _ => Combinator::Any,
                };
                let state = Rc::new(Aggregate {
                    values: RefCell::new(vec![Value::Undefined; items.len()]),
                    remaining: Cell::new(1),
                    promise: result.clone(),
                    combinator,
                });
                for (index, item) in items.into_iter().enumerate() {
                    let promise = self.promise_resolve(item)?;
                    state.remaining.set(state.remaining.get() + 1);
                    let element = |rejected| {
                        Self::native_value(PromiseFunction::Element {
                            state: state.clone(),
                            index,
                            rejected,
                        })
                    };
                    let (fulfilled, rejected) = match combinator {
                        Combinator::All => (
                            element(false),
                            Value::Builtin(Builtin::PromiseRejectPending(result.clone())),
                        ),
                        Combinator::AllSettled => (element(false), element(true)),
                        Combinator::Any => (
                            Value::Builtin(Builtin::PromiseResolvePending(result.clone())),
                            element(true),
                        ),
                    };
                    self.promise_then(promise, Some(&fulfilled), Some(&rejected))?;
                }
                state.remaining.set(state.remaining.get() - 1);
                self.settle_aggregate(&state);
                Ok(Value::Promise(result))
            }
            PromiseFunction::Element {
                state,
                index,
                rejected,
            } => {
                let value = argument(&arguments, 0);
                let stored = match state.combinator {
                    Combinator::AllSettled => {
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
                self.settle_aggregate(&state);
                Ok(Value::Undefined)
            }
            PromiseFunction::WithResolvers => {
                let promise = Self::pending_promise();
                Ok(self.object_from_entries(vec![
                    ("promise".to_string(), Value::Promise(promise.clone())),
                    (
                        "resolve".to_string(),
                        Value::Builtin(Builtin::PromiseResolvePending(promise.clone())),
                    ),
                    (
                        "reject".to_string(),
                        Value::Builtin(Builtin::PromiseRejectPending(promise)),
                    ),
                ]))
            }
            PromiseFunction::Finally(promise) => {
                let callback = argument(&arguments, 0);
                if !is_callable(&callback) {
                    return self.promise_then(promise, Some(&callback), Some(&callback));
                }
                let fulfilled = Self::native_value(PromiseFunction::AfterFinally {
                    callback: callback.clone(),
                    rejected: false,
                });
                let rejected = Self::native_value(PromiseFunction::AfterFinally {
                    callback,
                    rejected: true,
                });
                self.promise_then(promise, Some(&fulfilled), Some(&rejected))
            }
            PromiseFunction::AfterFinally { callback, rejected } => {
                let outcome = argument(&arguments, 0);
                let result = self.call_function(callback, Value::Undefined, Vec::new())?;
                let waited = self.promise_resolve(result)?;
                let settle = Self::native_value(PromiseFunction::Settle {
                    value: outcome,
                    rejected,
                });
                self.promise_then(waited, Some(&settle), None)
            }
            PromiseFunction::Settle { value, rejected } => {
                if rejected {
                    return Err(self.throw_value(value));
                }
                Ok(value)
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
