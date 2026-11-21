// document.startViewTransition: the update callback runs at the next rendering opportunity and
// its promises settle as the spec orders them; the engine draws no snapshot animation, so a
// transition is finished as soon as its update is done

use super::object::{JsObject, ObjectClass, Property, new_object};
use super::operations::is_callable;
use super::realm::Native;
use super::*;
use std::cell::Cell;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TransitionStep {
    // the rendering opportunity came: run the update callback
    Run,
    // what the callback handed back fulfilled
    Updated,
    // the callback threw or what it handed back rejected
    Failed,
    Skip,
}

pub(super) struct ViewTransition {
    callback: Value,
    update_done: PromiseHandle,
    ready: PromiseHandle,
    finished: PromiseHandle,
    skipped: Cell<bool>,
}

fn step(transition: &Rc<ViewTransition>, step: TransitionStep) -> Native {
    Native::ViewTransition(transition.clone(), step)
}

impl ScriptRuntime {
    pub(super) fn start_view_transition(&mut self, argument: Value) -> Result<Value, ScriptError> {
        let callback = if is_callable(&argument) {
            argument
        } else if let Value::Object(_) = &argument {
            self.get_property(&argument, "update")?
        } else {
            Value::Undefined
        };
        let ready = Self::pending_promise();
        // a skipped transition's ready rejection is expected, not left unhandled
        ready.borrow_mut().handled = true;
        let transition = Rc::new(ViewTransition {
            callback,
            update_done: Self::pending_promise(),
            ready,
            finished: Self::pending_promise(),
            skipped: Cell::new(false),
        });
        // a newer transition skips the one still under way
        if let Some((_, previous)) = self.view_transition.take() {
            self.skip_view_transition(&previous);
        }
        let object = new_object(JsObject::with_prototype(
            Some(self.intrinsics.hosts["ViewTransition"].clone()),
            ObjectClass::Ordinary,
        ));
        {
            let mut object = object.borrow_mut();
            for (name, promise) in [
                ("updateCallbackDone", &transition.update_done),
                ("ready", &transition.ready),
                ("finished", &transition.finished),
            ] {
                object.define(
                    name.to_string(),
                    Property::hidden(Value::Promise(promise.clone())),
                );
            }
            let skip =
                self.native_function(step(&transition, TransitionStep::Skip), "skipTransition", 0);
            object.define("skipTransition".to_string(), Property::hidden(skip));
        }
        let id = self.next_task_id;
        self.next_task_id = self.next_task_id.saturating_add(1);
        self.tasks.push(ScheduledTask {
            id,
            due: self.clock.saturating_add(self.frame_interval),
            callback: Value::Builtin(Builtin::Native(step(&transition, TransitionStep::Run))),
            arguments: Vec::new(),
            kind: TaskKind::AnimationFrame,
        });
        let value = Value::Object(object);
        self.view_transition = Some((value.clone(), transition));
        Ok(value)
    }

    pub(super) fn active_view_transition(&self) -> Value {
        self.view_transition
            .as_ref()
            .map_or(Value::Null, |(object, _)| object.clone())
    }

    fn skip_view_transition(&mut self, transition: &Rc<ViewTransition>) {
        if transition.skipped.replace(true) {
            return;
        }
        let reason = dom_exception("AbortError", "Transition was skipped");
        self.settle_promise(&transition.ready, PromiseOutcome::Rejected(reason));
    }

    fn end_view_transition(&mut self, transition: &Rc<ViewTransition>) {
        if self
            .view_transition
            .as_ref()
            .is_some_and(|(_, current)| Rc::ptr_eq(current, transition))
        {
            self.view_transition = None;
        }
    }

    pub(super) fn step_view_transition(
        &mut self,
        transition: &Rc<ViewTransition>,
        which: TransitionStep,
        settled: Value,
    ) -> Result<Value, ScriptError> {
        match which {
            TransitionStep::Skip => self.skip_view_transition(transition),
            TransitionStep::Run => {
                let result = if is_callable(&transition.callback) {
                    self.call_function(transition.callback.clone(), Value::Undefined, Vec::new())
                } else {
                    Ok(Value::Undefined)
                };
                let waited = result.and_then(|value| self.promise_resolve(value));
                match waited {
                    Ok(promise) => {
                        let fulfilled = Value::Builtin(Builtin::Native(step(
                            transition,
                            TransitionStep::Updated,
                        )));
                        let rejected = Value::Builtin(Builtin::Native(step(
                            transition,
                            TransitionStep::Failed,
                        )));
                        self.promise_then(promise, Some(&fulfilled), Some(&rejected))?;
                    }
                    Err(error) => {
                        let reason = self.take_thrown(error);
                        return self.step_view_transition(
                            transition,
                            TransitionStep::Failed,
                            reason,
                        );
                    }
                }
            }
            TransitionStep::Updated => {
                self.settle_promise(
                    &transition.update_done,
                    PromiseOutcome::Fulfilled(Value::Undefined),
                );
                self.settle_promise(
                    &transition.ready,
                    PromiseOutcome::Fulfilled(Value::Undefined),
                );
                self.settle_promise(
                    &transition.finished,
                    PromiseOutcome::Fulfilled(Value::Undefined),
                );
                self.end_view_transition(transition);
            }
            TransitionStep::Failed => {
                self.settle_promise(
                    &transition.update_done,
                    PromiseOutcome::Rejected(settled.clone()),
                );
                self.settle_promise(&transition.ready, PromiseOutcome::Rejected(settled.clone()));
                self.settle_promise(&transition.finished, PromiseOutcome::Rejected(settled));
                self.end_view_transition(transition);
            }
        }
        Ok(Value::Undefined)
    }
}
