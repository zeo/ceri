// the parts of navigator and window an app reaches for beside the page itself:
// navigator.permissions, navigator.storage, navigator.locks, navigator.userActivation,
// navigator.sendBeacon, and the scheduler with postTask and yield

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::operations::{is_callable, is_primitive, plain_object};
use super::realm::{Builder, Native};
use super::*;

// how long a press or key keeps the page transiently activated, as in Chromium
const ACTIVATION_MILLISECONDS: u64 = 5000;

#[derive(Clone)]
pub(super) enum NavFunction {
    PermissionsQuery,
    StorageEstimate,
    StoragePersisted,
    LocksRequest,
    LocksQuery,
    HasBeenActive,
    IsActive,
    SendBeacon,
    PostTask,
    Yield,
    // a granted lock's callback runs
    GrantLock(Rc<LockRequest>),
    // the promise a lock's callback handed back settled, and the lock goes
    LockSettled(Rc<LockRequest>, bool),
    // a posted task's delay is over
    RunTask(Rc<PostedTask>),
    // the yield promise settles in a task of its own
    Resume(PromiseHandle),
    // the signal a posted task or lock request was given aborted
    Aborted(Rc<PostedTask>),
}

pub(super) struct LockRequest {
    name: String,
    shared: bool,
    callback: Value,
    result: PromiseHandle,
    granted: Cell<bool>,
}

pub(super) struct PostedTask {
    callback: Value,
    result: PromiseHandle,
    done: Cell<bool>,
    signal: Option<AbortSignalHandle>,
}

pub(super) struct NavigatorInterfaces {
    pub(super) permissions: ObjectHandle,
    pub(super) storage: ObjectHandle,
    pub(super) locks: ObjectHandle,
    pub(super) user_activation: ObjectHandle,
    permission_status: ObjectHandle,
}

use std::cell::Cell;

fn native(function: NavFunction) -> Native {
    Native::Nav(Rc::new(function))
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    event_target: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> NavigatorInterfaces {
    let plain = |prototype: &ObjectHandle, tag: &str| {
        let object = new_object(JsObject::with_prototype(
            Some(prototype.clone()),
            ObjectClass::Ordinary,
        ));
        object.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property {
                writable: false,
                ..Property::hidden(Value::String(tag.into()))
            },
        );
        object
    };
    let method = |target: &ObjectHandle, name: &str, function: NavFunction, length: u32| {
        let made = build.function(native(function), name, length);
        target
            .borrow_mut()
            .define(name.to_string(), Property::hidden(Value::Object(made)));
    };
    let permissions = plain(object_prototype, "Permissions");
    method(&permissions, "query", NavFunction::PermissionsQuery, 1);
    let storage = plain(object_prototype, "StorageManager");
    method(&storage, "estimate", NavFunction::StorageEstimate, 0);
    method(&storage, "persisted", NavFunction::StoragePersisted, 0);
    method(&storage, "persist", NavFunction::StoragePersisted, 0);
    let locks = plain(object_prototype, "LockManager");
    method(&locks, "request", NavFunction::LocksRequest, 2);
    method(&locks, "query", NavFunction::LocksQuery, 0);
    let user_activation = plain(object_prototype, "UserActivation");
    for (name, function) in [
        ("hasBeenActive", NavFunction::HasBeenActive),
        ("isActive", NavFunction::IsActive),
    ] {
        let get = build.function(native(function), &format!("get {name}"), 0);
        user_activation.borrow_mut().define(
            name.to_string(),
            Property {
                slot: Slot::Accessor {
                    get: Some(Value::Object(get)),
                    set: None,
                },
                enumerable: true,
                writable: false,
                configurable: true,
            },
        );
    }
    let scheduler = plain(object_prototype, "Scheduler");
    method(&scheduler, "postTask", NavFunction::PostTask, 1);
    method(&scheduler, "yield", NavFunction::Yield, 0);
    globals.push(("scheduler", Value::Object(scheduler)));
    let permission_status = plain(event_target, "PermissionStatus");
    NavigatorInterfaces {
        permissions,
        storage,
        locks,
        user_activation,
        permission_status,
    }
}

// what a permission name answers in ceri: what it grants, and what it has no way to give
fn permission_state(name: &str, notifications: &str) -> Option<&'static str> {
    Some(match name {
        "notifications" | "push" => match notifications {
            "granted" => "granted",
            "denied" => "denied",
            _ => "prompt",
        },
        "clipboard-read" | "clipboard-write" | "persistent-storage" => "granted",
        "geolocation"
        | "camera"
        | "microphone"
        | "midi"
        | "background-sync"
        | "background-fetch"
        | "screen-wake-lock"
        | "local-fonts"
        | "window-management"
        | "accelerometer"
        | "gyroscope"
        | "magnetometer"
        | "ambient-light-sensor"
        | "storage-access"
        | "top-level-storage-access"
        | "idle-detection"
        | "nfc"
        | "display-capture"
        | "payment-handler"
        | "speaker-selection" => "denied",
        _ => return None,
    })
}

impl ScriptRuntime {
    // a trusted press or key marks the page as activated by its user
    pub(crate) fn note_activation(&mut self) {
        self.activated_at = Some(self.clock);
    }

    // whether the user pressed or typed into the page in the last few seconds
    pub(super) fn transiently_active(&self) -> bool {
        self.activated_at
            .is_some_and(|at| self.clock.saturating_sub(at) <= ACTIVATION_MILLISECONDS)
    }

    pub(crate) fn fullscreen_requested(&self) -> bool {
        self.fullscreen_element.is_some()
    }

    // leaves fullscreen and tells the element that held it
    pub(crate) fn leave_fullscreen(&mut self) -> Result<bool, ScriptError> {
        let Some(node) = self.fullscreen_element.take() else {
            return Ok(false);
        };
        let event = self.new_event(
            "Event",
            "fullscreenchange".to_string(),
            true,
            false,
            true,
            true,
        );
        self.dispatch(&event, super::events::ListenerTarget::Node(node))?;
        Ok(true)
    }

    pub(super) fn request_fullscreen(&mut self, node: NodeId) -> Result<Value, ScriptError> {
        if !self.transiently_active() {
            return Ok(Value::Promise(
                self.rejected_promise("Permissions check failed"),
            ));
        }
        if self.fullscreen_element != Some(node) {
            self.fullscreen_element = Some(node);
            let event = self.new_event(
                "Event",
                "fullscreenchange".to_string(),
                true,
                false,
                true,
                true,
            );
            self.dispatch(&event, super::events::ListenerTarget::Node(node))?;
        }
        Ok(Value::Promise(self.fulfilled_promise(Value::Undefined)))
    }

    fn lock_info(&self, request: &LockRequest) -> Value {
        plain_object([
            (
                "name".to_string(),
                Value::String(request.name.clone().into()),
            ),
            (
                "mode".to_string(),
                Value::String(
                    if request.shared {
                        "shared"
                    } else {
                        "exclusive"
                    }
                    .into(),
                ),
            ),
            ("clientId".to_string(), Value::String("ceri".into())),
        ])
    }

    // grants every waiting request whose lock is free, in the order they were made
    fn grant_locks(&mut self) {
        let queue = self.lock_queue.borrow().clone();
        let mut granted_now = Vec::new();
        for (index, request) in queue.iter().enumerate() {
            if request.granted.get() {
                continue;
            }
            let held = queue
                .iter()
                .filter(|other| other.granted.get() && other.name == request.name);
            let earlier = queue[..index]
                .iter()
                .filter(|other| !other.granted.get() && other.name == request.name);
            let free = if request.shared {
                held.clone().all(|other| other.shared) && earlier.clone().all(|other| other.shared)
            } else {
                held.clone().next().is_none() && earlier.clone().next().is_none()
            };
            if free {
                request.granted.set(true);
                granted_now.push(request.clone());
            }
        }
        for request in granted_now {
            self.schedule_native_task(native(NavFunction::GrantLock(request)));
        }
    }

    fn request_lock(&mut self, arguments: Vec<Value>) -> Result<Value, ScriptError> {
        let mut arguments = arguments.into_iter();
        let name = arguments.next().unwrap_or(Value::Undefined);
        let name = self.coerce_string(name)?;
        let (options, callback) = match (arguments.next(), arguments.next()) {
            (Some(callback), None) => (Value::Undefined, callback),
            (Some(options), Some(callback)) => (options, callback),
            _ => (Value::Undefined, Value::Undefined),
        };
        if !is_callable(&callback) {
            return Ok(Value::Promise(self.rejected_promise("Failed to execute 'request' on 'LockManager': The callback provided as parameter 2 is not a function.")));
        }
        if name.starts_with('-') {
            let error = dom_exception("NotSupportedError", "Names starting with '-' are reserved.");
            return Ok(Value::Promise(self.rejected_promise_value(error)));
        }
        let option = |runtime: &mut Self, key: &str| -> Result<Value, ScriptError> {
            if is_primitive(&options) {
                return Ok(Value::Undefined);
            }
            runtime.get_property(&options, key)
        };
        let shared = match option(self, "mode")? {
            Value::Undefined => false,
            mode => {
                match self.coerce_string(mode)?.as_str() {
                    "shared" => true,
                    "exclusive" => false,
                    other => {
                        return Err(self.type_error(format!("The provided value '{other}' is not a valid enum value of type LockMode.")));
                    }
                }
            }
        };
        let if_available = option(self, "ifAvailable")?.truthy();
        let request = Rc::new(LockRequest {
            name,
            shared,
            callback,
            result: Self::pending_promise(),
            granted: Cell::new(false),
        });
        self.lock_queue.borrow_mut().push(request.clone());
        self.grant_locks();
        if if_available && !request.granted.get() {
            self.lock_queue
                .borrow_mut()
                .retain(|other| !Rc::ptr_eq(other, &request));
            let outcome = self.call_function(
                request.callback.clone(),
                Value::Undefined,
                vec![Value::Null],
            );
            match outcome {
                Ok(value) => self.resolve_promise(&request.result, value)?,
                Err(error) => {
                    let reason = self.take_thrown(error);
                    self.settle_promise(&request.result, PromiseOutcome::Rejected(reason));
                }
            }
        }
        Ok(Value::Promise(request.result.clone()))
    }

    fn release_lock(&mut self, request: &Rc<LockRequest>) {
        self.lock_queue
            .borrow_mut()
            .retain(|other| !Rc::ptr_eq(other, request));
        self.grant_locks();
    }

    pub(super) fn call_nav(
        &mut self,
        function: NavFunction,
        _this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        match function {
            NavFunction::PermissionsQuery => {
                if is_primitive(&first) {
                    return Ok(Value::Promise(self.rejected_promise("Failed to execute 'query' on 'Permissions': parameter 1 is not of type 'object'.")));
                }
                let name = self.get_property(&first, "name")?;
                let name = self.coerce_string(name)?;
                let Some(state) = permission_state(&name, self.notice_permission()) else {
                    return Ok(Value::Promise(self.rejected_promise(format!("Failed to execute 'query' on 'Permissions': The provided value '{name}' is not a valid enum value of type PermissionName."))));
                };
                let prototype = self.intrinsics.navigator_apis.permission_status.clone();
                let status = self.new_event_target(prototype);
                {
                    let mut status = status.borrow_mut();
                    status.insert("name".to_string(), Value::String(name.into()));
                    status.insert("state".to_string(), Value::String(state.into()));
                    status.insert("onchange".to_string(), Value::Null);
                }
                Ok(Value::Promise(
                    self.fulfilled_promise(Value::Object(status)),
                ))
            }
            NavFunction::StorageEstimate => {
                let usage = match self.lookup_global("localStorage") {
                    Some(Value::Storage(storage)) => storage.borrow().byte_size(),
                    _ => 0,
                };
                let estimate = plain_object([
                    ("usage".to_string(), Value::Number(usage as f64)),
                    (
                        "quota".to_string(),
                        Value::Number(MAX_STORAGE_FILE_BYTES as f64),
                    ),
                ]);
                Ok(Value::Promise(self.fulfilled_promise(estimate)))
            }
            // an app's storage lives in its own profile, which nothing clears behind its back
            NavFunction::StoragePersisted => {
                Ok(Value::Promise(self.fulfilled_promise(Value::Bool(true))))
            }
            NavFunction::LocksRequest => self.request_lock(arguments),
            NavFunction::LocksQuery => {
                let queue = self.lock_queue.borrow().clone();
                let (held, pending): (Vec<_>, Vec<_>) =
                    queue.iter().partition(|request| request.granted.get());
                let held = held.iter().map(|request| self.lock_info(request)).collect();
                let pending = pending
                    .iter()
                    .map(|request| self.lock_info(request))
                    .collect();
                let snapshot = plain_object([
                    ("held".to_string(), Self::new_array(held)),
                    ("pending".to_string(), Self::new_array(pending)),
                ]);
                Ok(Value::Promise(self.fulfilled_promise(snapshot)))
            }
            NavFunction::GrantLock(request) => {
                let lock = plain_object([
                    (
                        "name".to_string(),
                        Value::String(request.name.clone().into()),
                    ),
                    (
                        "mode".to_string(),
                        Value::String(
                            if request.shared {
                                "shared"
                            } else {
                                "exclusive"
                            }
                            .into(),
                        ),
                    ),
                ]);
                let lock = self.host_object(lock, "Lock");
                let outcome =
                    self.call_function(request.callback.clone(), Value::Undefined, vec![lock]);
                let promise = match outcome {
                    Ok(value) => self.promise_resolve(value)?,
                    Err(error) => {
                        let reason = self.take_thrown(error);
                        self.rejected_promise_value(reason)
                    }
                };
                let fulfilled = self.native_function(
                    native(NavFunction::LockSettled(request.clone(), true)),
                    "",
                    1,
                );
                let rejected =
                    self.native_function(native(NavFunction::LockSettled(request, false)), "", 1);
                self.promise_then(promise, Some(&fulfilled), Some(&rejected))?;
                Ok(Value::Undefined)
            }
            NavFunction::LockSettled(request, fulfilled) => {
                self.release_lock(&request);
                let outcome = if fulfilled {
                    PromiseOutcome::Fulfilled(first)
                } else {
                    PromiseOutcome::Rejected(first)
                };
                self.settle_promise(&request.result, outcome);
                Ok(Value::Undefined)
            }
            NavFunction::HasBeenActive => Ok(Value::Bool(self.activated_at.is_some())),
            NavFunction::IsActive => {
                Ok(Value::Bool(self.activated_at.is_some_and(|at| {
                    self.clock.saturating_sub(at) <= ACTIVATION_MILLISECONDS
                })))
            }
            NavFunction::SendBeacon => {
                let fetch = self.get_global("fetch")?;
                let mut init = vec![
                    ("method".to_string(), Value::String("POST".into())),
                    ("keepalive".to_string(), Value::Bool(true)),
                ];
                if let Some(body) = arguments
                    .get(1)
                    .filter(|body| !matches!(body, Value::Undefined | Value::Null))
                {
                    init.push(("body".to_string(), body.clone()));
                }
                let sent =
                    self.call_function(fetch, Value::Undefined, vec![first, plain_object(init)])?;
                if let Value::Promise(sent) = sent {
                    sent.borrow_mut().handled = true;
                }
                Ok(Value::Bool(true))
            }
            NavFunction::PostTask => {
                if !is_callable(&first) {
                    return Err(self.type_error("Failed to execute 'postTask' on 'Scheduler': The callback provided as parameter 1 is not a function."));
                }
                let options = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                let (delay, signal) = if is_primitive(&options) {
                    (0.0, Value::Undefined)
                } else {
                    let delay = match self.get_property(&options, "delay")? {
                        Value::Undefined => 0.0,
                        value => self.coerce_number(value)?,
                    };
                    (delay, self.get_property(&options, "signal")?)
                };
                let signal = match signal {
                    Value::AbortSignal(signal) => Some(signal),
                    _ => None,
                };
                let task = Rc::new(PostedTask {
                    callback: first,
                    result: Self::pending_promise(),
                    done: Cell::new(false),
                    signal: signal.clone(),
                });
                if let Some(signal) = &signal {
                    if signal.borrow().aborted {
                        let reason = signal
                            .borrow()
                            .reason
                            .clone()
                            .unwrap_or_else(abort_error_value);
                        return Ok(Value::Promise(self.rejected_promise_value(reason)));
                    }
                    let listener =
                        self.native_function(native(NavFunction::Aborted(task.clone())), "", 1);
                    signal.borrow_mut().listeners.push(listener);
                }
                let delay = if delay.is_finite() && delay > 0.0 {
                    delay as u64
                } else {
                    0
                };
                let id = self.next_task_id;
                self.next_task_id = self.next_task_id.saturating_add(1);
                self.tasks.push(ScheduledTask {
                    id,
                    due: self.clock.saturating_add(delay),
                    callback: Value::Builtin(Builtin::Native(native(NavFunction::RunTask(
                        task.clone(),
                    )))),
                    arguments: Vec::new(),
                    kind: TaskKind::Timeout,
                });
                Ok(Value::Promise(task.result.clone()))
            }
            NavFunction::RunTask(task) => {
                if task.done.replace(true) {
                    return Ok(Value::Undefined);
                }
                match self.call_function(task.callback.clone(), Value::Undefined, Vec::new()) {
                    Ok(value) => self.resolve_promise(&task.result, value)?,
                    Err(error) => {
                        let reason = self.take_thrown(error);
                        self.settle_promise(&task.result, PromiseOutcome::Rejected(reason));
                    }
                }
                Ok(Value::Undefined)
            }
            NavFunction::Aborted(task) => {
                if !task.done.replace(true) {
                    let reason = task
                        .signal
                        .as_ref()
                        .and_then(|signal| signal.borrow().reason.clone())
                        .unwrap_or_else(abort_error_value);
                    self.settle_promise(&task.result, PromiseOutcome::Rejected(reason));
                }
                Ok(Value::Undefined)
            }
            NavFunction::Yield => {
                let promise = Self::pending_promise();
                self.schedule_native_task(native(NavFunction::Resume(promise.clone())));
                Ok(Value::Promise(promise))
            }
            NavFunction::Resume(promise) => {
                self.settle_promise(&promise, PromiseOutcome::Fulfilled(Value::Undefined));
                Ok(Value::Undefined)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigator_apis_answer_and_locks_take_turns() {
        let document =
            crate::dom::Document::parse(r#"<p id="out">idle</p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                (async () => {
                    const clipboard = await navigator.permissions.query({ name: "clipboard-read" });
                    const camera = await navigator.permissions.query({ name: "camera" });
                    seen.push(clipboard.state, camera.state, clipboard instanceof EventTarget);
                    try { await navigator.permissions.query({ name: "nonsense" }); } catch (error) { seen.push(error.name); }
                    const estimate = await navigator.storage.estimate();
                    seen.push(typeof estimate.usage, estimate.quota > 0, await navigator.storage.persisted());
                    const order = [];
                    const first = navigator.locks.request("doc", async (lock) => {
                        order.push("a:" + lock.mode);
                        await new Promise((done) => setTimeout(done, 5));
                        order.push("a-done");
                        return "first";
                    });
                    const second = navigator.locks.request("doc", () => { order.push("b"); return "second"; });
                    const skipped = await navigator.locks.request("doc", { ifAvailable: true }, (lock) => lock === null ? "skipped" : "got");
                    const snapshot = await navigator.locks.query();
                    seen.push(skipped, snapshot.held.length + "/" + snapshot.pending.length);
                    seen.push(await first, await second, order.join(","));
                    seen.push(navigator.userActivation.hasBeenActive, navigator.userActivation.isActive);
                    const steps = [];
                    const later = scheduler.postTask(() => { steps.push("task"); return 7; }, { delay: 2 });
                    await scheduler.yield();
                    steps.push("yielded");
                    steps.push(await later);
                    const controller = new AbortController();
                    const dropped = scheduler.postTask(() => "ran", { delay: 50, signal: controller.signal });
                    controller.abort();
                    try { await dropped; } catch (error) { steps.push(error.name); }
                    seen.push(steps.join(","), typeof navigator.sendBeacon);
                    document.getElementById("out").textContent = seen.join("|");
                })();
                "#,
            )
            .expect("script should run");
        for _ in 0..20 {
            runtime.advance_time(5).expect("timers should run");
        }
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(
                "granted|denied|true|TypeError|number|true|true|skipped|1/1|first|second|a:exclusive,a-done,b|false|false|yielded,task,7,AbortError|function"
            )
        );
        runtime.set_pointer_button(0, true);
        runtime
            .execute("document.getElementById('out').textContent = [navigator.userActivation.hasBeenActive, navigator.userActivation.isActive].join();")
            .expect("script should run");
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some("true,true")
        );
    }
}
