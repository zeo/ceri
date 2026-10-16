// the bytecode interpreter: frames run in a loop without recursing on the Rust stack for
// JavaScript-to-JavaScript calls; generators and async functions suspend their frame and
// resume it later, and exceptions unwind through try handlers and finally blocks

use super::ast::FunctionKind;
use super::compiler::{self, Code, NO_TARGET, Op};
use super::object::{
    JsObject, ObjectClass, ObjectHandle, PRIVATE_PREFIX, Property, Slot, new_object,
};
use super::realm::Native;
use super::*;

pub(super) type ScopeRef = Rc<RefCell<Scope>>;

pub(super) struct Scope {
    pub(super) slots: Vec<Value>,
    pub(super) parent: Option<ScopeRef>,
}

impl Scope {
    pub(super) fn new(size: usize, parent: Option<ScopeRef>) -> ScopeRef {
        Rc::new(RefCell::new(Scope {
            slots: vec![Value::Undefined; size],
            parent,
        }))
    }
}

#[derive(Clone)]
pub(super) enum FieldKind {
    Field(Option<Value>),
    Method(Value),
    Getter(Value),
    Setter(Value),
    StaticBlock(Value),
}

#[derive(Clone)]
pub(super) struct FieldDefinition {
    pub(super) key: String,
    pub(super) private: bool,
    pub(super) kind: FieldKind,
}

pub(super) struct Closure {
    pub(super) code: Rc<Code>,
    pub(super) scope: Option<ScopeRef>,
    pub(super) home: RefCell<Option<ObjectHandle>>,
    // instance fields and private methods a class constructor installs on each instance
    pub(super) fields: RefCell<Vec<FieldDefinition>>,
    pub(super) static_fields: RefCell<Vec<FieldDefinition>>,
    pub(super) url: Option<AppUrl>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ResumeMode {
    Next,
    Throw,
    Return,
}

pub(super) enum GeneratorState {
    Suspended(Box<Frame>),
    Executing,
    Completed,
}

pub(super) struct AsyncGeneratorRequest {
    pub(super) mode: ResumeMode,
    pub(super) value: Value,
    pub(super) promise: PromiseHandle,
}

pub(super) struct AsyncGeneratorData {
    pub(super) state: GeneratorState,
    pub(super) queue: VecDeque<AsyncGeneratorRequest>,
    // a frame suspended at an await rather than a yield
    pub(super) awaiting: bool,
}

pub(super) struct AsyncFunctionData {
    pub(super) frame: Option<Box<Frame>>,
    pub(super) promise: PromiseHandle,
    // set when the function is an async generator waiting on an await
    pub(super) generator: Option<Rc<RefCell<AsyncGeneratorData>>>,
}

#[derive(Clone)]
struct Handler {
    catch: u32,
    finally: u32,
    stack: usize,
    scopes: u16,
    chains: usize,
}

#[derive(Clone)]
enum Pending {
    Normal,
    Throw(Value),
    Return(Value),
    Jump {
        target: u32,
        handlers: u16,
        scopes: u16,
    },
}

pub(super) struct Frame {
    code: Rc<Code>,
    closure: Option<Rc<Closure>>,
    pc: usize,
    stack: Vec<Value>,
    scope: Option<ScopeRef>,
    scopes: u16,
    this: Value,
    new_target: Value,
    function: Value,
    arguments: Vec<Value>,
    handlers: Vec<Handler>,
    pending: Vec<Pending>,
    chains: Vec<usize>,
    construct: bool,
    // a resume value injected into a suspended yield or await
    resume: Option<(ResumeMode, Value)>,
    // the outstanding resume mode for yield*, so DelegateStep forwards throw and return
    delegate_mode: ResumeMode,
    // the mode of the last async yield* call, which decides how its result unpacks
    delegate_last: ResumeMode,
}

pub(super) enum RunResult {
    Return(Value),
    Yield(Value, Box<Frame>),
    Await(Value, Box<Frame>),
}

const MAX_FRAMES: usize = 10_000;
// the parent constructor of a derived class, in a slot scripts cannot name
const SUPER_KEY: &str = "\u{10ffff}#super";
const MAX_NESTED_RUNS: usize = 400;

impl ScriptRuntime {
    // ---- entry points ----

    pub(super) fn run_script_code(&mut self, code: Rc<Code>) -> Result<Value, ScriptError> {
        let scope = Scope::new(code.scope_size as usize, None);
        let frame = Frame::new(
            code,
            None,
            Some(scope),
            Value::Window,
            Value::Undefined,
            Value::Undefined,
            Vec::new(),
        );
        match self.run(frame)? {
            RunResult::Return(value) => Ok(value),
            _ => Err(runtime_error("top-level script code cannot suspend")),
        }
    }

    pub(super) fn run_module_code(
        &mut self,
        code: Rc<Code>,
        scope: ScopeRef,
        url: AppUrl,
    ) -> Result<Value, ScriptError> {
        let closure = Rc::new(Closure {
            code: code.clone(),
            scope: None,
            home: RefCell::new(None),
            fields: RefCell::new(Vec::new()),
            static_fields: RefCell::new(Vec::new()),
            url: Some(url),
        });
        let mut frame = Frame::new(
            code,
            Some(closure),
            None,
            Value::Undefined,
            Value::Undefined,
            Value::Undefined,
            Vec::new(),
        );
        frame.scope = Some(scope);
        // modules may await at the top level; the rest of the module runs as microtasks settle
        match self.run(frame)? {
            RunResult::Return(value) => Ok(value),
            RunResult::Await(value, frame) => {
                let promise = Rc::new(RefCell::new(PromiseData {
                    outcome: PromiseOutcome::Pending,
                    reactions: Vec::new(),
                }));
                let state = Rc::new(RefCell::new(AsyncFunctionData {
                    frame: Some(frame),
                    promise: promise.clone(),
                    generator: None,
                }));
                self.await_then_resume(value, state)?;
                self.drain_microtasks()?;
                Ok(Value::Promise(promise))
            }
            RunResult::Yield(..) => Err(runtime_error("modules cannot yield")),
        }
    }

    pub(super) fn new_closure(
        &self,
        code: Rc<Code>,
        scope: Option<ScopeRef>,
        url: Option<AppUrl>,
    ) -> Value {
        let closure = Rc::new(Closure {
            code,
            scope,
            home: RefCell::new(None),
            fields: RefCell::new(Vec::new()),
            static_fields: RefCell::new(Vec::new()),
            url,
        });
        self.function_object(closure)
    }

    // the function object around a closure, with length, name and a prototype where one applies
    pub(super) fn function_object(&self, closure: Rc<Closure>) -> Value {
        let code = closure.code.clone();
        let prototype = if code.is_async && !code.is_generator {
            self.intrinsics.async_function_prototype.clone()
        } else if code.is_generator {
            if code.is_async {
                self.intrinsics.async_generator_function_prototype.clone()
            } else {
                self.intrinsics.generator_function_prototype.clone()
            }
        } else {
            self.intrinsics.function_prototype.clone()
        };
        let mut object = JsObject::with_prototype(Some(prototype), ObjectClass::Function(closure));
        object.define(
            "length".to_string(),
            Property {
                slot: Slot::Data(Value::Number(f64::from(code.length))),
                enumerable: false,
                writable: false,
                configurable: true,
            },
        );
        object.define(
            "name".to_string(),
            Property {
                slot: Slot::Data(Value::String(code.name.to_string().into())),
                enumerable: false,
                writable: false,
                configurable: true,
            },
        );
        let constructible = matches!(
            code.kind,
            FunctionKind::Normal
                | FunctionKind::ClassConstructor
                | FunctionKind::DerivedConstructor
        );
        let handle = new_object(object);
        if code.is_generator {
            let proto = if code.is_async {
                &self.intrinsics.async_generator_prototype
            } else {
                &self.intrinsics.generator_prototype
            };
            let instance_proto = new_object(JsObject::with_prototype(
                Some(proto.clone()),
                ObjectClass::Ordinary,
            ));
            handle.borrow_mut().define(
                "prototype".to_string(),
                Property {
                    slot: Slot::Data(Value::Object(instance_proto)),
                    enumerable: false,
                    writable: true,
                    configurable: false,
                },
            );
        } else if constructible && !code.is_async {
            let proto = new_object(JsObject::with_prototype(
                Some(self.intrinsics.object_prototype.clone()),
                ObjectClass::Ordinary,
            ));
            proto.borrow_mut().define(
                "constructor".to_string(),
                Property::hidden(Value::Object(handle.clone())),
            );
            handle.borrow_mut().define(
                "prototype".to_string(),
                Property {
                    slot: Slot::Data(Value::Object(proto)),
                    enumerable: false,
                    writable: !matches!(
                        code.kind,
                        FunctionKind::ClassConstructor | FunctionKind::DerivedConstructor
                    ),
                    configurable: false,
                },
            );
        }
        Value::Object(handle)
    }

    // ---- calls ----

    // calls any callable value with a this binding, running JavaScript to completion
    pub(super) fn call_function(
        &mut self,
        callee: Value,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        match self.prepare_call(callee, this, arguments, None)? {
            Prepared::Done(value) => Ok(value),
            Prepared::Frame(frame) => self.run_to_value(*frame),
        }
    }

    pub(super) fn construct(
        &mut self,
        callee: Value,
        arguments: Vec<Value>,
        new_target: Option<Value>,
    ) -> Result<Value, ScriptError> {
        let new_target = new_target.unwrap_or_else(|| callee.clone());
        match self.prepare_construct(callee, arguments, new_target)? {
            Prepared::Done(value) => Ok(value),
            Prepared::Frame(frame) => self.run_to_value(*frame),
        }
    }

    fn run_to_value(&mut self, frame: Frame) -> Result<Value, ScriptError> {
        match self.run(frame)? {
            RunResult::Return(value) => Ok(value),
            _ => Err(runtime_error("a plain call cannot suspend")),
        }
    }

    pub(super) fn closure_of(value: &Value) -> Option<Rc<Closure>> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Function(closure) => Some(closure.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn prepare_call(
        &mut self,
        callee: Value,
        this: Value,
        arguments: Vec<Value>,
        _new_target: Option<Value>,
    ) -> Result<Prepared, ScriptError> {
        let callee = callee.resolve();
        if let Value::Object(object) = &callee {
            let class = object.borrow().class.clone();
            match class {
                ObjectClass::Function(closure) => {
                    if matches!(
                        closure.code.kind,
                        FunctionKind::ClassConstructor | FunctionKind::DerivedConstructor
                    ) {
                        return Err(self.type_error(format!(
                            "class constructor {} cannot be invoked without 'new'",
                            closure.code.name
                        )));
                    }
                    let this = if closure.code.strict || closure.code.kind == FunctionKind::Arrow {
                        this
                    } else if matches!(this, Value::Undefined | Value::Null) {
                        Value::Window
                    } else {
                        this
                    };
                    if closure.code.is_generator {
                        let frame = self.function_frame(
                            &closure,
                            &callee,
                            this,
                            Value::Undefined,
                            arguments,
                            false,
                        );
                        return Ok(Prepared::Done(
                            self.make_generator(&callee, &closure, frame),
                        ));
                    }
                    if closure.code.is_async {
                        let frame = self.function_frame(
                            &closure,
                            &callee,
                            this,
                            Value::Undefined,
                            arguments,
                            false,
                        );
                        return Ok(Prepared::Done(self.start_async(frame)?));
                    }
                    let frame = self.function_frame(
                        &closure,
                        &callee,
                        this,
                        Value::Undefined,
                        arguments,
                        false,
                    );
                    return Ok(Prepared::Frame(Box::new(frame)));
                }
                ObjectClass::Bound {
                    target,
                    this,
                    arguments: bound,
                } => {
                    let mut all = bound;
                    all.extend(arguments);
                    return self.prepare_call(target, this, all, None);
                }
                ObjectClass::Native(native) => {
                    return Ok(Prepared::Done(self.call_native(native, this, arguments)?));
                }
                ObjectClass::Proxy(proxy) => {
                    if !proxy.callable {
                        return Err(self.type_error("value is not a function"));
                    }
                    let (target, trap) = self.proxy_trap(&proxy, "apply")?;
                    return match trap {
                        Some(trap) => {
                            let handler =
                                proxy.handler.borrow().clone().unwrap_or(Value::Undefined);
                            let list = Self::new_array(arguments);
                            Ok(Prepared::Done(self.call_function(
                                trap,
                                handler,
                                vec![target, this, list],
                            )?))
                        }
                        None => self.prepare_call(target, this, arguments, None),
                    };
                }
                _ => {
                    return Err(self.type_error("value is not a function"));
                }
            }
        }
        Ok(Prepared::Done(self.call_host(callee, this, arguments)?))
    }

    fn prepare_construct(
        &mut self,
        callee: Value,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Prepared, ScriptError> {
        let callee = callee.resolve();
        if let Value::Object(object) = &callee {
            let class = object.borrow().class.clone();
            match class {
                ObjectClass::Function(closure) => {
                    let kind = closure.code.kind;
                    if !matches!(
                        kind,
                        FunctionKind::Normal
                            | FunctionKind::ClassConstructor
                            | FunctionKind::DerivedConstructor
                    ) || closure.code.is_async
                        || closure.code.is_generator
                    {
                        return Err(
                            self.type_error(format!("{} is not a constructor", closure.code.name))
                        );
                    }
                    let this = if kind == FunctionKind::DerivedConstructor {
                        Value::Uninitialized
                    } else {
                        let prototype = self.prototype_from_constructor(&new_target)?;
                        let instance = Value::Object(new_object(JsObject::with_prototype(
                            Some(prototype),
                            ObjectClass::Ordinary,
                        )));
                        if kind == FunctionKind::ClassConstructor {
                            self.initialize_fields(&closure, &instance)?;
                        }
                        instance
                    };
                    let frame =
                        self.function_frame(&closure, &callee, this, new_target, arguments, true);
                    return Ok(Prepared::Frame(Box::new(frame)));
                }
                ObjectClass::Bound {
                    target,
                    arguments: bound,
                    ..
                } => {
                    let mut all = bound;
                    all.extend(arguments);
                    let new_target = if Rc::ptr_eq(
                        object,
                        match &new_target {
                            Value::Object(target) => target,
                            _ => object,
                        },
                    ) {
                        target.clone()
                    } else {
                        new_target
                    };
                    return self.prepare_construct(target, all, new_target);
                }
                ObjectClass::Native(native) => {
                    return Ok(Prepared::Done(
                        self.construct_native(native, arguments, new_target)?,
                    ));
                }
                ObjectClass::Proxy(proxy) => {
                    if !proxy.constructor {
                        return Err(self.type_error("value is not a constructor"));
                    }
                    let (target, trap) = self.proxy_trap(&proxy, "construct")?;
                    let new_target = match &new_target {
                        Value::Object(target_object) if Rc::ptr_eq(target_object, object) => {
                            target.clone()
                        }
                        _ => new_target,
                    };
                    return match trap {
                        Some(trap) => {
                            let handler =
                                proxy.handler.borrow().clone().unwrap_or(Value::Undefined);
                            let list = Self::new_array(arguments);
                            let result =
                                self.call_function(trap, handler, vec![target, list, new_target])?;
                            if super::operations::is_primitive(&result) {
                                return Err(
                                    self.type_error("proxy construct trap returned a non-object")
                                );
                            }
                            Ok(Prepared::Done(result))
                        }
                        None => self.prepare_construct(target, arguments, new_target),
                    };
                }
                _ => return Err(self.type_error("value is not a constructor")),
            }
        }
        Ok(Prepared::Done(self.construct_host(callee, arguments)?))
    }

    // the prototype for a new instance: new.target's prototype property, or Object.prototype
    pub(super) fn prototype_from_constructor(
        &mut self,
        new_target: &Value,
    ) -> Result<ObjectHandle, ScriptError> {
        match self.get_property(new_target, "prototype")? {
            Value::Object(prototype) => Ok(prototype),
            _ => Ok(self.intrinsics.object_prototype.clone()),
        }
    }

    pub(super) fn initialize_fields(
        &mut self,
        closure: &Rc<Closure>,
        instance: &Value,
    ) -> Result<(), ScriptError> {
        let fields = closure.fields.borrow().clone();
        for field in fields {
            self.install_field(&field, instance)?;
        }
        Ok(())
    }

    fn install_field(
        &mut self,
        field: &FieldDefinition,
        target: &Value,
    ) -> Result<(), ScriptError> {
        let Value::Object(object) = target else {
            return Err(self.type_error("fields need an object"));
        };
        match &field.kind {
            FieldKind::Field(initializer) => {
                let value = match initializer {
                    Some(initializer) => {
                        self.call_function(initializer.clone(), target.clone(), Vec::new())?
                    }
                    None => Value::Undefined,
                };
                if field.private {
                    if object.borrow().contains_key(&field.key) {
                        return Err(self.type_error("private field is already defined"));
                    }
                    object
                        .borrow_mut()
                        .define(field.key.clone(), Property::hidden(value));
                } else {
                    self.define_own(object, field.key.clone(), Property::data(value))?;
                }
            }
            FieldKind::Method(method) => {
                object.borrow_mut().define(
                    field.key.clone(),
                    Property {
                        slot: Slot::Data(method.clone()),
                        enumerable: false,
                        writable: false,
                        configurable: false,
                    },
                );
            }
            FieldKind::Getter(getter) => {
                self.merge_accessor(object, &field.key, Some(getter.clone()), None, false)
            }
            FieldKind::Setter(setter) => {
                self.merge_accessor(object, &field.key, None, Some(setter.clone()), false)
            }
            FieldKind::StaticBlock(block) => {
                self.call_function(block.clone(), target.clone(), Vec::new())?;
            }
        }
        Ok(())
    }

    pub(super) fn merge_accessor(
        &mut self,
        object: &ObjectHandle,
        key: &str,
        getter: Option<Value>,
        setter: Option<Value>,
        enumerable: bool,
    ) {
        let mut object = object.borrow_mut();
        let (mut get, mut set) = match object.property(key).map(|property| &property.slot) {
            Some(Slot::Accessor { get, set }) => (get.clone(), set.clone()),
            _ => (None, None),
        };
        if getter.is_some() {
            get = getter;
        }
        if setter.is_some() {
            set = setter;
        }
        object.define(
            key.to_string(),
            Property {
                slot: Slot::Accessor { get, set },
                enumerable,
                writable: false,
                configurable: true,
            },
        );
    }

    fn function_frame(
        &mut self,
        closure: &Rc<Closure>,
        function: &Value,
        this: Value,
        new_target: Value,
        arguments: Vec<Value>,
        construct: bool,
    ) -> Frame {
        let code = closure.code.clone();
        let parent = if code.self_named {
            let scope = Scope::new(1, closure.scope.clone());
            scope.borrow_mut().slots[0] = function.clone();
            Some(scope)
        } else {
            closure.scope.clone()
        };
        let scope = Scope::new(code.scope_size as usize, parent);
        // TDZ: lexical bindings start uninitialized; params and vars start undefined
        let mut frame = Frame::new(
            code.clone(),
            Some(closure.clone()),
            Some(scope.clone()),
            this,
            new_target,
            function.clone(),
            arguments,
        );
        frame.construct = construct;
        if code.simple_params {
            let mut scope = scope.borrow_mut();
            for (position, slot) in code.param_slots.iter().enumerate() {
                if let Some(slot) = slot {
                    scope.slots[*slot as usize] = frame
                        .arguments
                        .get(position)
                        .cloned()
                        .unwrap_or(Value::Undefined);
                }
            }
        } else {
            let arguments = Value::Array(Rc::new(RefCell::new(frame.arguments.clone())));
            frame.stack.push(arguments);
        }
        frame
    }

    // host callables: builtins and the constructors the runtime exposes
    fn call_host(
        &mut self,
        callee: Value,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        match callee {
            Value::Builtin(Builtin::Native(native)) => self.call_native(native, this, arguments),
            Value::Builtin(builtin) => self.call_builtin(builtin, arguments),
            other if is_callable(&other) => self.call_host_constructor(other, arguments),
            other => Err(self.type_error(format!("{} is not a function", other.string_value()))),
        }
    }

    fn construct_host(
        &mut self,
        callee: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        match callee {
            Value::Builtin(_) => Err(self.type_error("value is not a constructor")),
            other if is_callable(&other) => self.call_host_constructor(other, arguments),
            other => Err(self.type_error(format!("{} is not a constructor", other.string_value()))),
        }
    }

    pub(super) fn construct_error_object(
        &mut self,
        kind: &str,
        arguments: Vec<Value>,
        new_target: &Value,
    ) -> Result<Value, ScriptError> {
        let prototype = match new_target {
            Value::Object(_) => self.prototype_from_constructor(new_target)?,
            _ => self.error_prototype(kind),
        };
        let mut object = JsObject::with_prototype(Some(prototype), ObjectClass::Error);
        if let Some(message) = arguments
            .first()
            .filter(|value| !matches!(value, Value::Undefined))
        {
            let message = self.coerce_string(message.clone())?;
            object.define(
                "message".to_string(),
                Property::hidden(Value::String(message.into())),
            );
        }
        if let Some(Value::Object(options)) = arguments.get(1)
            && let Some(cause) = options.borrow().get("cause")
        {
            object.define("cause".to_string(), Property::hidden(cause.clone()));
        }
        object.define(
            "stack".to_string(),
            Property::hidden(Value::String(String::new().into())),
        );
        Ok(Value::Object(new_object(object)))
    }

    // ---- the run loop ----

    pub(super) fn run(&mut self, frame: Frame) -> Result<RunResult, ScriptError> {
        if self.nested_runs >= MAX_NESTED_RUNS {
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        self.nested_runs += 1;
        let result = self.run_frames(frame);
        self.nested_runs -= 1;
        result
    }

    fn run_frames(&mut self, entry: Frame) -> Result<RunResult, ScriptError> {
        let mut frames = vec![entry];
        loop {
            self.steps += 1;
            if self.step_limit.is_some_and(|limit| self.steps > limit) {
                self.interrupted = true;
            }
            // an interrupted run leaves every frame at once, past any catch or finally
            if self.interrupted {
                return Err(super::runtime_error("script exceeded its step limit"));
            }
            let outcome = self.step(&mut frames);
            match outcome {
                Ok(Some(result)) => return Ok(result),
                Ok(None) => {}
                Err(error) if self.interrupted => return Err(error),
                Err(error) => {
                    let thrown = self.take_thrown(error);
                    if let Some(result) = self.unwind(&mut frames, thrown)? {
                        return Ok(result);
                    }
                }
            }
        }
    }

    // throws a value inside the frame stack: the nearest handler takes it, or the run ends
    fn unwind(
        &mut self,
        frames: &mut Vec<Frame>,
        thrown: Value,
    ) -> Result<Option<RunResult>, ScriptError> {
        loop {
            let Some(frame) = frames.last_mut() else {
                return Err(self.throw_value(thrown));
            };
            if let Some(handler) = frame.handlers.pop() {
                frame.stack.truncate(handler.stack);
                Self::restore_scopes(frame, handler.scopes);
                frame.chains.truncate(handler.chains);
                if handler.catch != NO_TARGET {
                    frame.stack.push(thrown);
                    frame.pc = handler.catch as usize;
                } else {
                    frame.pending.push(Pending::Throw(thrown));
                    frame.pc = handler.finally as usize;
                }
                return Ok(None);
            }
            if frames.len() == 1 {
                return Err(self.throw_value(thrown));
            }
            frames.pop();
        }
    }

    fn restore_scopes(frame: &mut Frame, target: u16) {
        while frame.scopes > target {
            let parent = frame
                .scope
                .as_ref()
                .and_then(|scope| scope.borrow().parent.clone());
            frame.scope = parent;
            frame.scopes -= 1;
        }
    }

    fn scope_at(frame: &Frame, depth: u16) -> ScopeRef {
        let mut scope = frame.scope.clone().expect("a scope is present");
        for _ in 0..depth {
            let parent = scope.borrow().parent.clone().expect("scope depth is valid");
            scope = parent;
        }
        scope
    }

    // returns from the top frame, running finally blocks on the way out
    fn do_return(
        &mut self,
        frames: &mut Vec<Frame>,
        value: Value,
    ) -> Result<Option<RunResult>, ScriptError> {
        let frame = frames.last_mut().expect("a frame is running");
        while let Some(handler) = frame.handlers.pop() {
            if handler.finally != NO_TARGET {
                frame.stack.truncate(handler.stack);
                Self::restore_scopes(frame, handler.scopes);
                frame.chains.truncate(handler.chains);
                frame.pending.push(Pending::Return(value));
                frame.pc = handler.finally as usize;
                return Ok(None);
            }
        }
        let finished = frames.pop().expect("a frame is running");
        let value = if finished.construct {
            match value {
                Value::Object(_) => value,
                _ if finished.code.kind == FunctionKind::DerivedConstructor
                    && !matches!(value, Value::Undefined) =>
                {
                    return Err(self.type_error(
                        "derived constructors may only return an object or undefined",
                    ));
                }
                _ => {
                    if matches!(finished.this, Value::Uninitialized) {
                        return Err(
                            self.reference_error("must call super constructor before returning")
                        );
                    }
                    finished.this
                }
            }
        } else {
            value
        };
        match frames.last_mut() {
            Some(caller) => {
                caller.stack.push(value);
                Ok(None)
            }
            None => Ok(Some(RunResult::Return(value))),
        }
    }

    // leaves try regions down to a depth, running each finally block before the jump lands
    fn do_exit(frame: &mut Frame, target: u32, handlers: u16, scopes: u16) {
        while frame.handlers.len() > handlers as usize {
            let handler = frame.handlers.pop().expect("checked above");
            if handler.finally != NO_TARGET {
                frame.stack.truncate(handler.stack);
                Self::restore_scopes(frame, handler.scopes);
                frame.chains.truncate(handler.chains);
                frame.pending.push(Pending::Jump {
                    target,
                    handlers,
                    scopes,
                });
                frame.pc = handler.finally as usize;
                return;
            }
        }
        Self::restore_scopes(frame, scopes);
        frame.pc = target as usize;
    }

    fn pop(frame: &mut Frame) -> Value {
        frame.stack.pop().unwrap_or(Value::Undefined)
    }

    fn pop_arguments(frame: &mut Frame, count: usize) -> Vec<Value> {
        let start = frame.stack.len().saturating_sub(count);
        frame.stack.split_off(start)
    }

    fn spread_list(&mut self, value: Value) -> Result<Vec<Value>, ScriptError> {
        match value {
            Value::Array(array) => Ok(array.borrow().clone()),
            other => self.iterate_to_vec(other),
        }
    }

    // one instruction; Some ends this run with a result
    fn step(&mut self, frames: &mut Vec<Frame>) -> Result<Option<RunResult>, ScriptError> {
        let frame = frames.last_mut().expect("a frame is running");
        let op = frame.code.ops[frame.pc];
        frame.pc += 1;
        match op {
            Op::Nop => {}
            Op::Undefined => frame.stack.push(Value::Undefined),
            Op::Null => frame.stack.push(Value::Null),
            Op::True => frame.stack.push(Value::Bool(true)),
            Op::False => frame.stack.push(Value::Bool(false)),
            Op::Number(value) => frame.stack.push(Value::Number(value)),
            Op::BigInt(index) => {
                let value = super::bigint::BigInt::parse(&frame.code.names[index as usize])
                    .expect("the lexer produced decimal digits");
                frame.stack.push(Value::BigInt(Rc::new(value)));
            }
            Op::String(index) => {
                let text = frame.code.names[index as usize].to_string();
                frame.stack.push(Value::String(text.into()));
            }
            Op::Regex { pattern, flags } => {
                let pattern = frame.code.names[pattern as usize].to_string();
                let flags = frame.code.names[flags as usize].to_string();
                let regex = self.new_regexp(&pattern, &flags)?;
                frames.last_mut().expect("frame").stack.push(regex);
            }
            Op::TemplateObject(index) => {
                let code = frame.code.clone();
                let value = self.template_object(&code, index as usize);
                frames.last_mut().expect("frame").stack.push(value);
            }
            Op::Pop => {
                frame.stack.pop();
            }
            Op::Dup => {
                let value = frame.stack.last().cloned().unwrap_or(Value::Undefined);
                frame.stack.push(value);
            }
            Op::Dup2 => {
                let length = frame.stack.len();
                let pair = frame.stack[length - 2..].to_vec();
                frame.stack.extend(pair);
            }
            Op::Dup3 => {
                let length = frame.stack.len();
                let triple = frame.stack[length - 3..].to_vec();
                frame.stack.extend(triple);
            }
            Op::RotateUp4 => {
                let length = frame.stack.len();
                let value = frame.stack.remove(length - 4);
                frame.stack.push(value);
            }
            Op::RotateDown5 => {
                let value = Self::pop(frame);
                let length = frame.stack.len();
                frame.stack.insert(length - 4, value);
            }
            Op::Swap => {
                let length = frame.stack.len();
                frame.stack.swap(length - 1, length - 2);
            }
            Op::RotateDown3 => {
                let value = Self::pop(frame);
                let length = frame.stack.len();
                frame.stack.insert(length - 2, value);
            }
            Op::RotateDown4 => {
                let value = Self::pop(frame);
                let length = frame.stack.len();
                frame.stack.insert(length - 3, value);
            }
            Op::RotateUp3 => {
                let length = frame.stack.len();
                let value = frame.stack.remove(length - 3);
                frame.stack.push(value);
            }
            Op::RequireCoercible => {
                if matches!(
                    frame.stack.last(),
                    Some(Value::Undefined | Value::Null) | None
                ) {
                    return Err(self.type_error("cannot destructure null or undefined"));
                }
            }
            Op::CopyRest => {
                let keys = Self::pop(frame);
                let source = Self::pop(frame);
                let excluded: Vec<String> = match keys {
                    Value::Array(keys) => keys.borrow().iter().map(Value::property_key).collect(),
                    _ => Vec::new(),
                };
                let copy = self.new_plain_object();
                for (key, value) in self.own_enumerable_entries(&source)? {
                    if !excluded.contains(&key) {
                        copy.borrow_mut().insert(key, value);
                    }
                }
                frames
                    .last_mut()
                    .expect("frame")
                    .stack
                    .push(Value::Object(copy));
            }
            Op::GetLocal { depth, index, name } => {
                let scope = Self::scope_at(frame, depth);
                let value = scope.borrow().slots[index as usize].clone();
                let value = match value {
                    Value::Binding(cell, _) => cell.borrow().clone(),
                    value => value,
                };
                if matches!(value, Value::Uninitialized) {
                    let name = frame.code.names[name as usize].clone();
                    return Err(
                        self.reference_error(format!("cannot access {name} before initialization"))
                    );
                }
                frame.stack.push(value);
            }
            Op::SetLocal { depth, index, name } => {
                let value = frame.stack.last().cloned().unwrap_or(Value::Undefined);
                let scope = Self::scope_at(frame, depth);
                let mut scope = scope.borrow_mut();
                let slot = &mut scope.slots[index as usize];
                match slot {
                    Value::Binding(cell, true) => {
                        if matches!(*cell.borrow(), Value::Uninitialized) {
                            let name = frame.code.names[name as usize].clone();
                            drop(scope);
                            return Err(self.reference_error(format!(
                                "cannot access {name} before initialization"
                            )));
                        }
                        *cell.borrow_mut() = value;
                    }
                    Value::Binding(_, false) => {
                        let name = frame.code.names[name as usize].clone();
                        drop(scope);
                        return Err(
                            self.type_error(format!("Assignment to constant variable '{name}'"))
                        );
                    }
                    Value::Uninitialized => {
                        let name = frame.code.names[name as usize].clone();
                        drop(scope);
                        return Err(self.reference_error(format!(
                            "cannot access {name} before initialization"
                        )));
                    }
                    slot => *slot = value,
                }
            }
            Op::InitLocal { depth, index } => {
                let value = Self::pop(frame);
                let scope = Self::scope_at(frame, depth);
                let mut scope = scope.borrow_mut();
                match &scope.slots[index as usize] {
                    Value::Binding(cell, _) => *cell.borrow_mut() = value,
                    _ => scope.slots[index as usize] = value,
                }
            }
            Op::GetGlobal(name) => {
                let name = frame.code.names[name as usize].clone();
                let value = self.get_global(&name)?;
                frames.last_mut().expect("frame").stack.push(value);
            }
            Op::TypeofGlobal(name) => {
                let name = frame.code.names[name as usize].clone();
                let value = self.lookup_global(&name).unwrap_or(Value::Undefined);
                let kind = self.type_of(&value);
                frames
                    .last_mut()
                    .expect("frame")
                    .stack
                    .push(Value::String(kind.to_string().into()));
            }
            Op::SetGlobal(name) | Op::InitGlobal(name) => {
                let name = frame.code.names[name as usize].clone();
                let value = frame.stack.last().cloned().unwrap_or(Value::Undefined);
                let strict = frame.code.strict;
                let initialize = matches!(op, Op::InitGlobal(_));
                self.set_global(&name, value, strict && !initialize)?;
            }
            Op::InitGlobalConst(name) => {
                let name = frame.code.names[name as usize].to_string();
                let value = frame.stack.last().cloned().unwrap_or(Value::Undefined);
                self.global
                    .borrow_mut()
                    .values
                    .insert(name, Value::Binding(Rc::new(RefCell::new(value)), false));
            }
            Op::DeclareGlobal(name) => {
                let name = frame.code.names[name as usize].clone();
                if self.lookup_global(&name).is_none() {
                    self.global
                        .borrow_mut()
                        .values
                        .insert(name.to_string(), Value::Undefined);
                }
            }
            Op::DeleteGlobal(name) => {
                let name = frame.code.names[name as usize].clone();
                self.global.borrow_mut().values.remove(&*name);
                frames
                    .last_mut()
                    .expect("frame")
                    .stack
                    .push(Value::Bool(true));
            }
            Op::PushScope(size) => {
                let scope = Scope::new(size as usize, frame.scope.clone());
                // lexical bindings in a block start in their temporal dead zone
                for slot in scope.borrow_mut().slots.iter_mut() {
                    *slot = Value::Uninitialized;
                }
                frame.scope = Some(scope);
                frame.scopes += 1;
            }
            Op::PopScope => {
                Self::restore_scopes(frame, frame.scopes.saturating_sub(1));
            }
            Op::CloneScope => {
                let scope = frame.scope.clone().expect("a scope to copy");
                let copy = Scope::new(0, scope.borrow().parent.clone());
                copy.borrow_mut().slots = scope.borrow().slots.clone();
                frame.scope = Some(copy);
            }
            Op::This => {
                if matches!(frame.this, Value::Uninitialized) {
                    return Err(
                        self.reference_error("must call super constructor before accessing this")
                    );
                }
                frame.stack.push(frame.this.clone());
            }
            Op::GlobalThis => frame.stack.push(Value::Window),
            Op::NewTarget => frame.stack.push(frame.new_target.clone()),
            Op::Home => {
                let home = frame
                    .closure
                    .as_ref()
                    .and_then(|closure| closure.home.borrow().clone())
                    .map(Value::Object)
                    .unwrap_or(Value::Undefined);
                frame.stack.push(home);
            }
            Op::ImportMeta => {
                let url = frame
                    .closure
                    .as_ref()
                    .and_then(|closure| closure.url.clone());
                let meta = self.new_plain_object();
                if let Some(url) = url {
                    meta.borrow_mut()
                        .insert("url".to_string(), Value::String(url.to_string().into()));
                }
                frames
                    .last_mut()
                    .expect("frame")
                    .stack
                    .push(Value::Object(meta));
            }
            Op::InitThisSlot(slot) => {
                let value = frame.this.clone();
                Self::scope_at(frame, 0).borrow_mut().slots[slot as usize] = value;
            }
            Op::InitNewTargetSlot(slot) => {
                let value = frame.new_target.clone();
                Self::scope_at(frame, 0).borrow_mut().slots[slot as usize] = value;
            }
            Op::InitHomeSlot(slot) => {
                let home = frame
                    .closure
                    .as_ref()
                    .and_then(|closure| closure.home.borrow().clone())
                    .map(Value::Object)
                    .unwrap_or(Value::Undefined);
                Self::scope_at(frame, 0).borrow_mut().slots[slot as usize] = home;
            }
            Op::InitArgumentsSlot(slot) => {
                let arguments = frame.arguments.clone();
                let object = self.arguments_object(arguments);
                let frame = frames.last_mut().expect("frame");
                Self::scope_at(frame, 0).borrow_mut().slots[slot as usize] = object;
            }
            Op::NewObject => {
                let object = self.new_plain_object();
                frames
                    .last_mut()
                    .expect("frame")
                    .stack
                    .push(Value::Object(object));
            }
            Op::NewArray => frame
                .stack
                .push(Value::Array(Rc::new(RefCell::new(Vec::new())))),
            Op::ArrayPush => {
                let value = Self::pop(frame);
                if let Some(Value::Array(array)) = frame.stack.last() {
                    array.borrow_mut().push(value);
                }
            }
            Op::ArrayHole => {
                if let Some(Value::Array(array)) = frame.stack.last() {
                    array.borrow_mut().push(Value::Undefined);
                }
            }
            Op::ArraySpread => {
                let value = Self::pop(frame);
                let items = self.spread_list(value)?;
                let frame = frames.last_mut().expect("frame");
                if let Some(Value::Array(array)) = frame.stack.last() {
                    let mut array = array.borrow_mut();
                    if array.len() + items.len() > MAX_RUNTIME_ARRAY_LENGTH {
                        return Err(self.range_error("array exceeds the length limit"));
                    }
                    array.extend(items);
                }
            }
            Op::DefineField(name) => {
                let value = Self::pop(frame);
                let key = frame.code.names[name as usize].to_string();
                if let Some(Value::Object(object)) = frame.stack.last() {
                    object.borrow_mut().define(key, Property::data(value));
                }
            }
            Op::DefineComputed => {
                let value = Self::pop(frame);
                let key = Self::pop(frame).property_key();
                if let Some(Value::Object(object)) = frame.stack.last() {
                    object.borrow_mut().define(key, Property::data(value));
                }
            }
            Op::DefineAccessor {
                getter,
                computed,
                name,
            } => {
                let function = Self::pop(frame);
                let key = if computed {
                    Self::pop(frame).property_key()
                } else {
                    frame.code.names[name as usize].to_string()
                };
                let Some(Value::Object(object)) = frame.stack.last().cloned() else {
                    return Err(runtime_error("accessor target is not an object"));
                };
                Self::set_home(&function, &object);
                if getter {
                    self.merge_accessor(&object, &key, Some(function), None, true);
                } else {
                    self.merge_accessor(&object, &key, None, Some(function), true);
                }
            }
            Op::ObjectSpread => {
                let source = Self::pop(frame);
                let entries = self.own_enumerable_entries(&source)?;
                let frame = frames.last_mut().expect("frame");
                if let Some(Value::Object(object)) = frame.stack.last() {
                    let mut object = object.borrow_mut();
                    for (key, value) in entries {
                        object.define(key, Property::data(value));
                    }
                }
            }
            Op::SetPrototype => {
                let prototype = Self::pop(frame);
                if let Some(Value::Object(object)) = frame.stack.last() {
                    match prototype {
                        Value::Object(prototype) => {
                            object.borrow_mut().set_prototype(Some(prototype))
                        }
                        Value::Null => object.borrow_mut().set_prototype(None),
                        _ => {}
                    }
                }
            }
            Op::GetProp(name) => {
                let object = Self::pop(frame);
                let key = frame.code.names[name as usize].clone();
                let value = self.get_property(&object, &key)?;
                frames.last_mut().expect("frame").stack.push(value);
            }
            Op::GetIndex => {
                let key = Self::pop(frame);
                let object = Self::pop(frame);
                let value = self.get_index(&object, key)?;
                frames.last_mut().expect("frame").stack.push(value);
            }
            Op::SetProp(name) => {
                let value = Self::pop(frame);
                let object = Self::pop(frame);
                let key = frame.code.names[name as usize].clone();
                let strict = frame.code.strict;
                self.set_property(&object, &key, value.clone(), strict)?;
                frames.last_mut().expect("frame").stack.push(value);
            }
            Op::SetIndex => {
                let value = Self::pop(frame);
                let key = Self::pop(frame);
                let object = Self::pop(frame);
                let strict = frame.code.strict;
                self.set_index(&object, key, value.clone(), strict)?;
                frames.last_mut().expect("frame").stack.push(value);
            }
            Op::DeleteProp(name) => {
                let object = Self::pop(frame);
                let key = frame.code.names[name as usize].clone();
                let strict = frame.code.strict;
                let deleted = self.delete_property(&object, &key, strict)?;
                frames
                    .last_mut()
                    .expect("frame")
                    .stack
                    .push(Value::Bool(deleted));
            }
            Op::DeleteIndex => {
                let key = Self::pop(frame);
                let object = Self::pop(frame);
                let strict = frame.code.strict;
                let key = self.coerce_key(key)?;
                let deleted = self.delete_property(&object, &key, strict)?;
                frames
                    .last_mut()
                    .expect("frame")
                    .stack
                    .push(Value::Bool(deleted));
            }
            Op::GetSuper(name) => {
                let home = Self::pop(frame);
                let this = frame.stack.last().cloned().unwrap_or(Value::Undefined);
                let key = frame.code.names[name as usize].clone();
                let value = self.get_super(&home, &this, &key)?;
                frames.last_mut().expect("frame").stack.push(value);
            }
            Op::GetSuperIndex => {
                let key = Self::pop(frame);
                let home = Self::pop(frame);
                let this = frame.stack.last().cloned().unwrap_or(Value::Undefined);
                let key = self.coerce_key(key)?;
                let value = self.get_super(&home, &this, &key)?;
                frames.last_mut().expect("frame").stack.push(value);
            }
            Op::SetSuper(name) => {
                let value = Self::pop(frame);
                let _home = Self::pop(frame);
                let this = Self::pop(frame);
                let key = frame.code.names[name as usize].clone();
                self.set_property(&this, &key, value.clone(), true)?;
                frames.last_mut().expect("frame").stack.push(value);
            }
            Op::SetSuperIndex => {
                let value = Self::pop(frame);
                let key = Self::pop(frame);
                let _home = Self::pop(frame);
                let this = Self::pop(frame);
                self.set_index(&this, key, value.clone(), true)?;
                frames.last_mut().expect("frame").stack.push(value);
            }
            Op::GetPrivate => {
                let key = Self::pop(frame).property_key();
                let object = Self::pop(frame);
                let value = self.get_private(&object, &key)?;
                frames.last_mut().expect("frame").stack.push(value);
            }
            Op::SetPrivate => {
                let value = Self::pop(frame);
                let key = Self::pop(frame).property_key();
                let object = Self::pop(frame);
                self.set_private(&object, &key, value.clone())?;
                frames.last_mut().expect("frame").stack.push(value);
            }
            Op::HasPrivate => {
                let object = Self::pop(frame);
                let key = Self::pop(frame).property_key();
                let Value::Object(object) = object else {
                    return Err(self.type_error("private brand checks need an object"));
                };
                let has = object.borrow().contains_key(&key);
                frames
                    .last_mut()
                    .expect("frame")
                    .stack
                    .push(Value::Bool(has));
            }
            Op::NewPrivateName(name) => {
                self.private_names += 1;
                let key = format!(
                    "{}{}:{}",
                    PRIVATE_PREFIX, self.private_names, frame.code.names[name as usize]
                );
                frame.stack.push(Value::String(key.into()));
            }
            Op::Call(count) | Op::CallMethod(count) => {
                let arguments = Self::pop_arguments(frame, count as usize);
                let callee = Self::pop(frame);
                let this = if matches!(op, Op::CallMethod(_)) {
                    Self::pop(frame)
                } else {
                    Value::Undefined
                };
                return self.call_in_loop(frames, callee, this, arguments);
            }
            Op::CallSpread | Op::CallMethodSpread => {
                let list = Self::pop(frame);
                let callee = Self::pop(frame);
                let this = if matches!(op, Op::CallMethodSpread) {
                    Self::pop(frame)
                } else {
                    Value::Undefined
                };
                let arguments = match list {
                    Value::Array(array) => array.borrow().clone(),
                    _ => Vec::new(),
                };
                return self.call_in_loop(frames, callee, this, arguments);
            }
            Op::New(count) => {
                let arguments = Self::pop_arguments(frame, count as usize);
                let callee = Self::pop(frame);
                return self.construct_in_loop(frames, callee, arguments, None);
            }
            Op::NewSpread => {
                let list = Self::pop(frame);
                let callee = Self::pop(frame);
                let arguments = match list {
                    Value::Array(array) => array.borrow().clone(),
                    _ => Vec::new(),
                };
                return self.construct_in_loop(frames, callee, arguments, None);
            }
            Op::SuperCall(count) => {
                let arguments = Self::pop_arguments(frame, count as usize);
                return self.super_call(frames, arguments);
            }
            Op::SuperCallSpread => {
                let list = Self::pop(frame);
                let arguments = match list {
                    Value::Array(array) => array.borrow().clone(),
                    _ => Vec::new(),
                };
                return self.super_call(frames, arguments);
            }
            Op::Closure(index) | Op::ClosureNamed(index) => {
                let code = frame.code.children[index as usize].clone();
                let url = frame
                    .closure
                    .as_ref()
                    .and_then(|closure| closure.url.clone());
                let scope = frame.scope.clone();
                let home = frame
                    .closure
                    .as_ref()
                    .and_then(|closure| closure.home.borrow().clone());
                let function = self.new_closure(code.clone(), scope, url);
                // arrows and nested methods keep the home object of where they were made
                if code.kind == FunctionKind::Arrow
                    && let (Some(home), Some(closure)) = (home, Self::closure_of(&function))
                {
                    *closure.home.borrow_mut() = Some(home);
                }
                frames.last_mut().expect("frame").stack.push(function);
            }
            Op::Class {
                template,
                has_super,
            } => {
                let template = frame.code.classes[template as usize].clone();
                let superclass = if has_super {
                    Some(Self::pop(frame))
                } else {
                    None
                };
                let url = frame
                    .closure
                    .as_ref()
                    .and_then(|closure| closure.url.clone());
                let scope = frame.scope.clone();
                let (constructor, prototype) =
                    self.create_class(&template, superclass, scope, url)?;
                let frame = frames.last_mut().expect("frame");
                frame.stack.push(constructor);
                frame.stack.push(prototype);
            }
            Op::DefineMethod {
                is_static,
                kind,
                computed,
                name,
            } => {
                let function = Self::pop(frame);
                let key = if computed {
                    Self::pop(frame).property_key()
                } else {
                    frame.code.names[name as usize].to_string()
                };
                let length = frame.stack.len();
                let (Value::Object(constructor), Value::Object(prototype)) = (
                    frame.stack[length - 2].clone(),
                    frame.stack[length - 1].clone(),
                ) else {
                    return Err(runtime_error("method target is not a class"));
                };
                let target = if is_static {
                    constructor.clone()
                } else {
                    prototype
                };
                Self::set_home(&function, &target);
                self.define_method(&constructor, &target, key, kind, function, is_static)?;
            }
            Op::DefineLiteralMethod { computed, name } => {
                let function = Self::pop(frame);
                let key = if computed {
                    Self::pop(frame).property_key()
                } else {
                    frame.code.names[name as usize].to_string()
                };
                let Some(Value::Object(object)) = frame.stack.last().cloned() else {
                    return Err(runtime_error("method target is not an object"));
                };
                Self::set_home(&function, &object);
                object.borrow_mut().define(key, Property::data(function));
            }
            Op::AddField {
                is_static,
                private,
                computed,
                name,
            } => {
                let initializer = Self::pop(frame);
                let key = if name == u32::MAX {
                    String::new()
                } else if computed {
                    Self::pop(frame).property_key()
                } else {
                    frame.code.names[name as usize].to_string()
                };
                let length = frame.stack.len();
                let (Value::Object(constructor), Value::Object(prototype)) = (
                    frame.stack[length - 2].clone(),
                    frame.stack[length - 1].clone(),
                ) else {
                    return Err(runtime_error("field target is not a class"));
                };
                let home = if is_static {
                    constructor.clone()
                } else {
                    prototype
                };
                Self::set_home(&initializer, &home);
                let Some(closure) = Self::closure_of(&Value::Object(constructor)) else {
                    return Err(runtime_error("field target is not a class"));
                };
                let kind = if name == u32::MAX {
                    FieldKind::StaticBlock(initializer)
                } else if matches!(initializer, Value::Undefined) {
                    FieldKind::Field(None)
                } else {
                    FieldKind::Field(Some(initializer))
                };
                let definition = FieldDefinition { key, private, kind };
                if is_static {
                    closure.static_fields.borrow_mut().push(definition);
                } else {
                    closure.fields.borrow_mut().push(definition);
                }
            }
            Op::RunStaticInit => {
                let constructor = frame.stack.last().cloned().unwrap_or(Value::Undefined);
                if let Some(closure) = Self::closure_of(&constructor) {
                    let statics = std::mem::take(&mut *closure.static_fields.borrow_mut());
                    for field in statics {
                        self.install_field(&field, &constructor)?;
                    }
                }
            }
            Op::Binary(operator) => {
                let right = Self::pop(frame);
                let left = Self::pop(frame);
                let value = self.binary(operator, left, right)?;
                frames.last_mut().expect("frame").stack.push(value);
            }
            Op::Negate => {
                let value = Self::pop(frame);
                let result = self.negate_numeric(value)?;
                frames.last_mut().expect("frame").stack.push(result);
            }
            Op::Plus => {
                let value = Self::pop(frame);
                let number = self.coerce_number(value)?;
                frames
                    .last_mut()
                    .expect("frame")
                    .stack
                    .push(Value::Number(number));
            }
            Op::ToNumeric => {
                let value = Self::pop(frame);
                let numeric = self.coerce_numeric(value)?;
                frames.last_mut().expect("frame").stack.push(numeric);
            }
            Op::Not => {
                let value = Self::pop(frame);
                frame.stack.push(Value::Bool(!value.truthy()));
            }
            Op::BitNot => {
                let value = Self::pop(frame);
                let result = match self.coerce_numeric(value)? {
                    Value::BigInt(value) => Value::BigInt(Rc::new(value.not())),
                    other => Value::Number(f64::from(!super::operations::to_int32_number(
                        self.coerce_number(other)?,
                    ))),
                };
                frames.last_mut().expect("frame").stack.push(result);
            }
            Op::TypeOf => {
                let value = Self::pop(frame);
                let kind = self.type_of(&value);
                frames
                    .last_mut()
                    .expect("frame")
                    .stack
                    .push(Value::String(kind.to_string().into()));
            }
            Op::ToPropertyKey => {
                let value = Self::pop(frame);
                let key = self.coerce_key(value)?;
                frames
                    .last_mut()
                    .expect("frame")
                    .stack
                    .push(Value::String(key.into()));
            }
            Op::Increment | Op::Decrement => {
                let value = Self::pop(frame);
                let delta = if matches!(op, Op::Increment) { 1 } else { -1 };
                let result = match value {
                    Value::Number(number) => Value::Number(number + f64::from(delta)),
                    other => match self.coerce_numeric(other)? {
                        Value::BigInt(value) => Value::BigInt(Rc::new(
                            value.add(&super::bigint::BigInt::from_i64(i64::from(delta))),
                        )),
                        numeric => Value::Number(self.coerce_number(numeric)? + f64::from(delta)),
                    },
                };
                frames.last_mut().expect("frame").stack.push(result);
            }
            Op::TemplateConcat(count) => {
                let parts = Self::pop_arguments(frame, count as usize);
                let mut text = String::new();
                for part in parts {
                    match part {
                        Value::String(part) => text.push_str(&part),
                        Value::Symbol(_) => {
                            return Err(self.type_error("cannot convert a symbol to a string"));
                        }
                        other => text.push_str(&self.coerce_string(other)?),
                    }
                }
                frames
                    .last_mut()
                    .expect("frame")
                    .stack
                    .push(Value::String(text.into()));
            }
            Op::Jump(target) => frame.pc = target as usize,
            Op::JumpIfFalse(target) => {
                if !Self::pop(frame).truthy() {
                    frame.pc = target as usize;
                }
            }
            Op::JumpIfTrue(target) => {
                if Self::pop(frame).truthy() {
                    frame.pc = target as usize;
                }
            }
            Op::JumpIfFalseKeep(target) => {
                if !frame.stack.last().is_some_and(Value::truthy) {
                    frame.pc = target as usize;
                }
            }
            Op::JumpIfTrueKeep(target) => {
                if frame.stack.last().is_some_and(Value::truthy) {
                    frame.pc = target as usize;
                }
            }
            Op::JumpIfNotNullishKeep(target) => {
                if !matches!(
                    frame.stack.last(),
                    Some(Value::Undefined | Value::Null) | None
                ) {
                    frame.pc = target as usize;
                }
            }
            Op::JumpIfNotUndefined(target) => {
                if !matches!(frame.stack.last(), Some(Value::Undefined) | None) {
                    frame.pc = target as usize;
                }
            }
            Op::ChainStart => frame.chains.push(frame.stack.len()),
            Op::JumpIfNullishChain(target) => {
                if matches!(
                    frame.stack.last(),
                    Some(Value::Undefined | Value::Null) | None
                ) {
                    let mark = frame.chains.pop().unwrap_or(frame.stack.len());
                    frame.stack.truncate(mark);
                    frame.stack.push(Value::Undefined);
                    frame.pc = target as usize;
                }
            }
            Op::ChainEnd => {
                frame.chains.pop();
            }
            Op::GetIterator | Op::GetAsyncIterator => {
                let value = Self::pop(frame);
                let iterator = if matches!(op, Op::GetAsyncIterator) {
                    self.get_async_iterator(value)?
                } else {
                    self.get_iterator(value)?
                };
                frames.last_mut().expect("frame").stack.push(iterator);
            }
            Op::IterValue => {
                let iterator = frame.stack.last().cloned().unwrap_or(Value::Undefined);
                let value = self.iterator_step(&iterator)?.unwrap_or(Value::Undefined);
                frames.last_mut().expect("frame").stack.push(value);
            }
            Op::IterRest => {
                let iterator = frame.stack.last().cloned().unwrap_or(Value::Undefined);
                let mut rest = Vec::new();
                while let Some(value) = self.iterator_step(&iterator)? {
                    rest.push(value);
                }
                frames
                    .last_mut()
                    .expect("frame")
                    .stack
                    .push(Value::Array(Rc::new(RefCell::new(rest))));
            }
            Op::IterClose => {
                let iterator = Self::pop(frame);
                self.iterator_close(&iterator)?;
            }
            Op::IterStep(target) => {
                let iterator = frame.stack.last().cloned().unwrap_or(Value::Undefined);
                match self.iterator_step(&iterator)? {
                    Some(value) => frames.last_mut().expect("frame").stack.push(value),
                    None => frames.last_mut().expect("frame").pc = target as usize,
                }
            }
            Op::AsyncIterNext => {
                let iterator = frame.stack.last().cloned().unwrap_or(Value::Undefined);
                let promise = self.async_iterator_next(&iterator)?;
                frames.last_mut().expect("frame").stack.push(promise);
            }
            Op::IterResultUnpack(target) => {
                let result = Self::pop(frame);
                if !matches!(result, Value::Object(_)) {
                    return Err(self.type_error("iterator result is not an object"));
                }
                let done = self.get_property(&result, "done")?.truthy();
                if done {
                    frames.last_mut().expect("frame").pc = target as usize;
                } else {
                    let value = self.get_property(&result, "value")?;
                    frames.last_mut().expect("frame").stack.push(value);
                }
            }
            Op::ForInPrepare => {
                let value = Self::pop(frame);
                let keys = self.for_in_keys(&value)?;
                let iterator = Value::VmIterator(Rc::new(RefCell::new(VmIterator::Keys {
                    keys,
                    index: 0,
                    object: value,
                })));
                frames.last_mut().expect("frame").stack.push(iterator);
            }
            Op::ForInNext(target) => {
                let iterator = frame.stack.last().cloned().unwrap_or(Value::Undefined);
                match self.for_in_step(&iterator)? {
                    Some(key) => frames
                        .last_mut()
                        .expect("frame")
                        .stack
                        .push(Value::String(key.into())),
                    None => frames.last_mut().expect("frame").pc = target as usize,
                }
            }
            Op::Throw => {
                let value = Self::pop(frame);
                return Err(self.throw_value(value));
            }
            Op::ThrowConstAssign(name) => {
                let name = frame.code.names[name as usize].clone();
                return Err(self.type_error(format!("Assignment to constant variable '{name}'")));
            }
            Op::PushHandler { catch, finally } => {
                let handler = Handler {
                    catch,
                    finally,
                    stack: frame.stack.len(),
                    scopes: frame.scopes,
                    chains: frame.chains.len(),
                };
                frame.handlers.push(handler);
            }
            Op::PopHandler => {
                frame.handlers.pop();
            }
            Op::CompletionNormal => frame.pending.push(Pending::Normal),
            Op::EndFinally => match frame.pending.pop().unwrap_or(Pending::Normal) {
                Pending::Normal => {}
                Pending::Throw(value) => {
                    return Err(self.throw_value(value));
                }
                Pending::Return(value) => return self.do_return(frames, value),
                Pending::Jump {
                    target,
                    handlers,
                    scopes,
                } => Self::do_exit(frame, target, handlers, scopes),
            },
            Op::Exit {
                target,
                handlers,
                scopes,
            } => Self::do_exit(frame, target, handlers, scopes),
            Op::Return => {
                let value = Self::pop(frame);
                return self.do_return(frames, value);
            }
            Op::Yield | Op::YieldDelegating => {
                if let Some((mode, value)) = frame.resume.take() {
                    // resuming: the sent value lands where the yield was
                    match (mode, matches!(op, Op::YieldDelegating)) {
                        (ResumeMode::Next, _) => frame.stack.push(value),
                        (mode, true) => {
                            frame.delegate_mode = mode;
                            frame.stack.push(value);
                        }
                        (ResumeMode::Throw, false) => {
                            return Err(self.throw_value(value));
                        }
                        (ResumeMode::Return, false) => return self.do_return(frames, value),
                    }
                    return Ok(None);
                }
                let value = Self::pop(frame);
                // the yield runs again on resume to receive the sent value
                frame.pc -= 1;
                if frames.len() != 1 {
                    return Err(runtime_error("yield outside a resumable frame"));
                }
                let frame = frames.pop().expect("generator frame");
                return Ok(Some(RunResult::Yield(value, Box::new(frame))));
            }
            Op::DelegateCall => {
                let sent = Self::pop(frame);
                let iterator = frame.stack.last().cloned().unwrap_or(Value::Undefined);
                let mode = std::mem::replace(&mut frame.delegate_mode, ResumeMode::Next);
                frame.delegate_last = mode;
                let promise = self.delegate_call(&iterator, mode, sent)?;
                frames.last_mut().expect("frame").stack.push(promise);
            }
            Op::DelegateUnpack(target) => {
                let result = Self::pop(frame);
                if super::operations::is_primitive(&result) {
                    return Err(self.type_error("iterator result is not an object"));
                }
                let done = self.get_property(&result, "done")?.truthy();
                let value = self.get_property(&result, "value")?;
                let frame = frames.last_mut().expect("frame");
                if done {
                    frame.stack.pop();
                    if frame.delegate_last == ResumeMode::Return {
                        return self.do_return(frames, value);
                    }
                    frame.stack.push(value);
                    frame.pc = target as usize;
                } else {
                    frame.stack.push(value);
                }
            }
            Op::DelegateStep(target) => {
                let sent = Self::pop(frame);
                let iterator = frame.stack.last().cloned().unwrap_or(Value::Undefined);
                let mode = std::mem::replace(&mut frame.delegate_mode, ResumeMode::Next);
                let result = self.delegate_resume(&iterator, mode, sent)?;
                let frame = frames.last_mut().expect("frame");
                match result {
                    DelegateResult::Yielded(value) => frame.stack.push(value),
                    DelegateResult::Done(value) => {
                        frame.stack.pop();
                        frame.stack.push(value);
                        frame.pc = target as usize;
                    }
                    DelegateResult::Returned(value) => {
                        frame.stack.pop();
                        return self.do_return(frames, value);
                    }
                }
            }
            Op::Await => {
                if let Some((mode, value)) = frame.resume.take() {
                    match mode {
                        ResumeMode::Next => frame.stack.push(value),
                        _ => {
                            return Err(self.throw_value(value));
                        }
                    }
                    return Ok(None);
                }
                let value = Self::pop(frame);
                frame.pc -= 1;
                if frames.len() != 1 {
                    return Err(runtime_error("await outside a resumable frame"));
                }
                let frame = frames.pop().expect("async frame");
                return Ok(Some(RunResult::Await(value, Box::new(frame))));
            }
            Op::DynamicImport => {
                let options = Self::pop(frame);
                let specifier = Self::pop(frame);
                let url = frame
                    .closure
                    .as_ref()
                    .and_then(|closure| closure.url.clone())
                    .or_else(|| self.base_url.clone());
                let promise = self.dynamic_import_promise(url, specifier, options);
                frames.last_mut().expect("frame").stack.push(promise);
            }
        }
        Ok(None)
    }

    fn set_home(function: &Value, home: &ObjectHandle) {
        if let Some(closure) = Self::closure_of(function) {
            *closure.home.borrow_mut() = Some(home.clone());
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn define_method(
        &mut self,
        constructor: &ObjectHandle,
        target: &ObjectHandle,
        key: String,
        kind: u8,
        function: Value,
        is_static: bool,
    ) -> Result<(), ScriptError> {
        match kind {
            compiler::METHOD => {
                target.borrow_mut().define(key, Property::hidden(function));
            }
            compiler::GETTER => self.merge_accessor(target, &key, Some(function), None, false),
            compiler::SETTER => self.merge_accessor(target, &key, None, Some(function), false),
            _ => {
                // private methods and accessors are installed on each instance, or on the class
                let field_kind = match kind {
                    compiler::PRIVATE_GETTER => FieldKind::Getter(function),
                    compiler::PRIVATE_SETTER => FieldKind::Setter(function),
                    _ => FieldKind::Method(function),
                };
                let definition = FieldDefinition {
                    key,
                    private: true,
                    kind: field_kind,
                };
                if is_static {
                    let target = Value::Object(constructor.clone());
                    self.install_field(&definition, &target)?;
                } else if let Some(closure) = Self::closure_of(&Value::Object(constructor.clone()))
                {
                    // methods come before fields, so they exist when initializers run
                    let mut fields = closure.fields.borrow_mut();
                    let position = fields
                        .iter()
                        .position(|field| matches!(field.kind, FieldKind::Field(_)))
                        .unwrap_or(fields.len());
                    fields.insert(position, definition);
                }
            }
        }
        Ok(())
    }

    fn create_class(
        &mut self,
        template: &Rc<compiler::ClassTemplate>,
        superclass: Option<Value>,
        scope: Option<ScopeRef>,
        url: Option<AppUrl>,
    ) -> Result<(Value, Value), ScriptError> {
        let (prototype_parent, constructor_parent) = match &superclass {
            None => (
                Some(self.intrinsics.object_prototype.clone()),
                self.intrinsics.function_prototype.clone(),
            ),
            Some(Value::Null) => (None, self.intrinsics.function_prototype.clone()),
            Some(superclass) => {
                if !Self::is_constructor(superclass) {
                    return Err(self.type_error("class extends value is not a constructor or null"));
                }
                let prototype = match self.get_property(superclass, "prototype")? {
                    Value::Object(prototype) => Some(prototype),
                    Value::Null => None,
                    _ => {
                        return Err(self.type_error("class extends value has an invalid prototype"));
                    }
                };
                let parent = match superclass {
                    Value::Object(parent) => parent.clone(),
                    // host constructors such as Error inherit through a stand-in function object
                    _ => self.intrinsics.function_prototype.clone(),
                };
                (prototype, parent)
            }
        };
        let constructor = self.new_closure(template.constructor.clone(), scope, url);
        let Value::Object(constructor_object) = &constructor else {
            unreachable!("closures are objects");
        };
        constructor_object
            .borrow_mut()
            .set_prototype(Some(constructor_parent));
        let prototype = new_object(JsObject::with_prototype(
            prototype_parent,
            ObjectClass::Ordinary,
        ));
        prototype.borrow_mut().define(
            "constructor".to_string(),
            Property::hidden(constructor.clone()),
        );
        constructor_object.borrow_mut().define(
            "prototype".to_string(),
            Property {
                slot: Slot::Data(Value::Object(prototype.clone())),
                enumerable: false,
                writable: false,
                configurable: false,
            },
        );
        if let Some(closure) = Self::closure_of(&constructor) {
            *closure.home.borrow_mut() = Some(prototype.clone());
        }
        if let Some(superclass) = superclass.filter(|value| !matches!(value, Value::Null)) {
            constructor_object
                .borrow_mut()
                .define(SUPER_KEY.to_string(), Property::hidden(superclass));
        }
        Ok((constructor, Value::Object(prototype)))
    }

    // super(...) in a derived constructor: construct the parent with this frame's new.target
    fn super_call(
        &mut self,
        frames: &mut [Frame],
        arguments: Vec<Value>,
    ) -> Result<Option<RunResult>, ScriptError> {
        let frame = frames.last_mut().expect("frame");
        let function = frame.function.clone();
        let new_target = frame.new_target.clone();
        let parent = match &function {
            Value::Object(object) => object
                .borrow()
                .property(SUPER_KEY)
                .and_then(|property| property.value().cloned()),
            _ => None,
        };
        let Some(parent) = parent else {
            return Err(self.type_error("super constructor is unavailable"));
        };
        if !matches!(frame.this, Value::Uninitialized) {
            return Err(self.reference_error("super constructor may only be called once"));
        }
        let this = self.construct(parent, arguments, Some(new_target))?;
        let frame = frames.last_mut().expect("frame");
        frame.this = this.clone();
        if let Some(closure) = frame.closure.clone() {
            self.initialize_fields(&closure, &this)?;
        }
        frames.last_mut().expect("frame").stack.push(this);
        Ok(None)
    }

    fn call_in_loop(
        &mut self,
        frames: &mut Vec<Frame>,
        callee: Value,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Option<RunResult>, ScriptError> {
        match self.prepare_call(callee, this, arguments, None)? {
            Prepared::Done(value) => frames.last_mut().expect("frame").stack.push(value),
            Prepared::Frame(frame) => {
                if frames.len() >= MAX_FRAMES {
                    return Err(self.range_error("Maximum call stack size exceeded"));
                }
                frames.push(*frame);
            }
        }
        Ok(None)
    }

    fn construct_in_loop(
        &mut self,
        frames: &mut Vec<Frame>,
        callee: Value,
        arguments: Vec<Value>,
        new_target: Option<Value>,
    ) -> Result<Option<RunResult>, ScriptError> {
        let new_target = new_target.unwrap_or_else(|| callee.clone());
        match self.prepare_construct(callee, arguments, new_target)? {
            Prepared::Done(value) => frames.last_mut().expect("frame").stack.push(value),
            Prepared::Frame(frame) => {
                if frames.len() >= MAX_FRAMES {
                    return Err(self.range_error("Maximum call stack size exceeded"));
                }
                frames.push(*frame);
            }
        }
        Ok(None)
    }

    // ---- generators ----

    fn make_generator(&mut self, function: &Value, closure: &Rc<Closure>, frame: Frame) -> Value {
        let prototype = match self.get_property(function, "prototype") {
            Ok(Value::Object(prototype)) => prototype,
            _ => {
                if closure.code.is_async {
                    self.intrinsics.async_generator_prototype.clone()
                } else {
                    self.intrinsics.generator_prototype.clone()
                }
            }
        };
        if closure.code.is_async {
            let data = Rc::new(RefCell::new(AsyncGeneratorData {
                state: GeneratorState::Suspended(Box::new(frame)),
                queue: VecDeque::new(),
                awaiting: false,
            }));
            return Value::Object(new_object(JsObject::with_prototype(
                Some(prototype),
                ObjectClass::AsyncGenerator(data),
            )));
        }
        let state = Rc::new(RefCell::new(GeneratorState::Suspended(Box::new(frame))));
        Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Generator(state),
        )))
    }

    // runs a generator to its next yield; the result is an iterator result object
    pub(super) fn generator_resume(
        &mut self,
        state: &Rc<RefCell<GeneratorState>>,
        mode: ResumeMode,
        value: Value,
    ) -> Result<Value, ScriptError> {
        let current = std::mem::replace(&mut *state.borrow_mut(), GeneratorState::Executing);
        let mut frame = match current {
            GeneratorState::Executing => {
                *state.borrow_mut() = GeneratorState::Executing;
                return Err(self.type_error("generator is already running"));
            }
            GeneratorState::Completed => {
                *state.borrow_mut() = GeneratorState::Completed;
                return match mode {
                    ResumeMode::Next => Ok(self.iterator_result(Value::Undefined, true)),
                    ResumeMode::Return => Ok(self.iterator_result(value, true)),
                    ResumeMode::Throw => Err(self.throw_value(value)),
                };
            }
            GeneratorState::Suspended(frame) => frame,
        };
        let started = frame.pc > 0;
        if !started {
            match mode {
                ResumeMode::Next => {}
                ResumeMode::Return => {
                    *state.borrow_mut() = GeneratorState::Completed;
                    return Ok(self.iterator_result(value, true));
                }
                ResumeMode::Throw => {
                    *state.borrow_mut() = GeneratorState::Completed;
                    return Err(self.throw_value(value));
                }
            }
        } else {
            frame.resume = Some((mode, value));
        }
        match self.run(*frame) {
            Ok(RunResult::Yield(value, frame)) => {
                *state.borrow_mut() = GeneratorState::Suspended(frame);
                Ok(self.iterator_result(value, false))
            }
            Ok(RunResult::Return(value)) => {
                *state.borrow_mut() = GeneratorState::Completed;
                Ok(self.iterator_result(value, true))
            }
            Ok(RunResult::Await(..)) => {
                *state.borrow_mut() = GeneratorState::Completed;
                Err(runtime_error("await in a synchronous generator"))
            }
            Err(error) => {
                *state.borrow_mut() = GeneratorState::Completed;
                Err(error)
            }
        }
    }

    // ---- async functions ----

    fn start_async(&mut self, frame: Frame) -> Result<Value, ScriptError> {
        let promise = Rc::new(RefCell::new(PromiseData {
            outcome: PromiseOutcome::Pending,
            reactions: Vec::new(),
        }));
        let state = Rc::new(RefCell::new(AsyncFunctionData {
            frame: Some(Box::new(frame)),
            promise: promise.clone(),
            generator: None,
        }));
        self.async_step(&state, None)?;
        Ok(Value::Promise(promise))
    }

    // runs an async function until it awaits or finishes, settling its promise at the end
    pub(super) fn async_step(
        &mut self,
        state: &Rc<RefCell<AsyncFunctionData>>,
        resume: Option<(ResumeMode, Value)>,
    ) -> Result<(), ScriptError> {
        let Some(mut frame) = state.borrow_mut().frame.take() else {
            return Ok(());
        };
        frame.resume = resume;
        let promise = state.borrow().promise.clone();
        match self.run(*frame) {
            Ok(RunResult::Return(value)) => {
                self.resolve_promise(&promise, value)?;
            }
            Ok(RunResult::Await(value, frame)) => {
                state.borrow_mut().frame = Some(frame);
                self.await_then_resume(value, state.clone())?;
            }
            Ok(RunResult::Yield(..)) => {
                self.settle_promise(
                    &promise,
                    PromiseOutcome::Rejected(Value::String(
                        "yield in an async function".to_string().into(),
                    )),
                );
            }
            Err(error) => {
                let reason = self.take_thrown(error);
                self.settle_promise(&promise, PromiseOutcome::Rejected(reason));
            }
        }
        Ok(())
    }

    // subscribes a suspended async frame to a value, resuming it as a microtask
    pub(super) fn await_then_resume(
        &mut self,
        value: Value,
        state: Rc<RefCell<AsyncFunctionData>>,
    ) -> Result<(), ScriptError> {
        let promise = self.promise_resolve(value)?;
        let fulfilled = Value::Builtin(Builtin::Native(Native::AsyncResume(
            AsyncResumeHandle(state.clone()),
            false,
        )));
        let rejected = Value::Builtin(Builtin::Native(Native::AsyncResume(
            AsyncResumeHandle(state),
            true,
        )));
        self.promise_then(promise, Some(&fulfilled), Some(&rejected))?;
        Ok(())
    }

    // ---- async generators ----

    pub(super) fn async_generator_enqueue(
        &mut self,
        data: &Rc<RefCell<AsyncGeneratorData>>,
        mode: ResumeMode,
        value: Value,
    ) -> Result<Value, ScriptError> {
        let promise = Rc::new(RefCell::new(PromiseData {
            outcome: PromiseOutcome::Pending,
            reactions: Vec::new(),
        }));
        data.borrow_mut().queue.push_back(AsyncGeneratorRequest {
            mode,
            value,
            promise: promise.clone(),
        });
        let idle = matches!(
            data.borrow().state,
            GeneratorState::Suspended(_) | GeneratorState::Completed
        ) && !data.borrow().awaiting;
        if idle {
            self.async_generator_drain(data)?;
        }
        Ok(Value::Promise(promise))
    }

    fn async_generator_drain(
        &mut self,
        data: &Rc<RefCell<AsyncGeneratorData>>,
    ) -> Result<(), ScriptError> {
        loop {
            let Some(request) = data.borrow_mut().queue.pop_front() else {
                return Ok(());
            };
            let state = std::mem::replace(&mut data.borrow_mut().state, GeneratorState::Executing);
            let frame = match state {
                GeneratorState::Completed => {
                    data.borrow_mut().state = GeneratorState::Completed;
                    match request.mode {
                        ResumeMode::Next => {
                            let result = self.iterator_result(Value::Undefined, true);
                            self.settle_promise(
                                &request.promise,
                                PromiseOutcome::Fulfilled(result),
                            );
                        }
                        ResumeMode::Return => {
                            let result = self.iterator_result(request.value, true);
                            self.settle_promise(
                                &request.promise,
                                PromiseOutcome::Fulfilled(result),
                            );
                        }
                        ResumeMode::Throw => {
                            self.settle_promise(
                                &request.promise,
                                PromiseOutcome::Rejected(request.value),
                            );
                        }
                    }
                    continue;
                }
                GeneratorState::Executing => return Ok(()),
                GeneratorState::Suspended(frame) => frame,
            };
            data.borrow_mut().queue.push_front(AsyncGeneratorRequest {
                mode: request.mode,
                value: Value::Undefined,
                promise: request.promise.clone(),
            });
            let started = frame.pc > 0;
            let resume = if started {
                Some((request.mode, request.value))
            } else if request.mode != ResumeMode::Next {
                data.borrow_mut().queue.pop_front();
                data.borrow_mut().state = GeneratorState::Completed;
                match request.mode {
                    ResumeMode::Return => {
                        let result = self.iterator_result(request.value, true);
                        self.settle_promise(&request.promise, PromiseOutcome::Fulfilled(result));
                    }
                    _ => self
                        .settle_promise(&request.promise, PromiseOutcome::Rejected(request.value)),
                }
                continue;
            } else {
                None
            };
            self.async_generator_run(data, frame, resume)?;
            if data.borrow().awaiting {
                return Ok(());
            }
        }
    }

    pub(super) fn async_generator_run(
        &mut self,
        data: &Rc<RefCell<AsyncGeneratorData>>,
        mut frame: Box<Frame>,
        resume: Option<(ResumeMode, Value)>,
    ) -> Result<(), ScriptError> {
        frame.resume = resume;
        let outcome = self.run(*frame);
        let request = data.borrow_mut().queue.pop_front();
        match outcome {
            Ok(RunResult::Yield(value, frame)) => {
                data.borrow_mut().state = GeneratorState::Suspended(frame);
                if let Some(request) = request {
                    let result = self.iterator_result(value, false);
                    self.settle_promise(&request.promise, PromiseOutcome::Fulfilled(result));
                }
            }
            Ok(RunResult::Return(value)) => {
                data.borrow_mut().state = GeneratorState::Completed;
                if let Some(request) = request {
                    let result = self.iterator_result(value, true);
                    self.settle_promise(&request.promise, PromiseOutcome::Fulfilled(result));
                }
            }
            Ok(RunResult::Await(value, frame)) => {
                // the request stays at the head of the queue until the frame yields
                if let Some(request) = request {
                    data.borrow_mut().queue.push_front(request);
                }
                data.borrow_mut().awaiting = true;
                let promise = Rc::new(RefCell::new(PromiseData {
                    outcome: PromiseOutcome::Pending,
                    reactions: Vec::new(),
                }));
                let state = Rc::new(RefCell::new(AsyncFunctionData {
                    frame: Some(frame),
                    promise,
                    generator: Some(data.clone()),
                }));
                self.await_then_resume(value, state)?;
            }
            Err(error) => {
                data.borrow_mut().state = GeneratorState::Completed;
                let reason = self.take_thrown(error);
                if let Some(request) = request {
                    self.settle_promise(&request.promise, PromiseOutcome::Rejected(reason));
                }
            }
        }
        Ok(())
    }

    // resumes a suspended async frame after the awaited promise settled
    pub(super) fn async_resume(
        &mut self,
        state: &Rc<RefCell<AsyncFunctionData>>,
        value: Value,
        rejected: bool,
    ) -> Result<(), ScriptError> {
        let mode = if rejected {
            ResumeMode::Throw
        } else {
            ResumeMode::Next
        };
        let generator = state.borrow().generator.clone();
        match generator {
            Some(data) => {
                let Some(frame) = state.borrow_mut().frame.take() else {
                    return Ok(());
                };
                data.borrow_mut().awaiting = false;
                self.async_generator_run(&data, frame, Some((mode, value)))?;
                if !data.borrow().awaiting {
                    self.async_generator_drain(&data)?;
                }
                Ok(())
            }
            None => self.async_step(state, Some((mode, value))),
        }
    }
}

pub(super) enum Prepared {
    Done(Value),
    Frame(Box<Frame>),
}

pub(super) enum DelegateResult {
    Yielded(Value),
    Done(Value),
    Returned(Value),
}

impl Frame {
    #[allow(clippy::too_many_arguments)]
    fn new(
        code: Rc<Code>,
        closure: Option<Rc<Closure>>,
        scope: Option<ScopeRef>,
        this: Value,
        new_target: Value,
        function: Value,
        arguments: Vec<Value>,
    ) -> Self {
        Frame {
            code,
            closure,
            pc: 0,
            stack: Vec::with_capacity(16),
            scope,
            scopes: 0,
            this,
            new_target,
            function,
            arguments,
            handlers: Vec::new(),
            pending: Vec::new(),
            chains: Vec::new(),
            construct: false,
            resume: None,
            delegate_mode: ResumeMode::Next,
            delegate_last: ResumeMode::Next,
        }
    }
}

// a wrapper so an async resumption can sit inside the Builtin enum
#[derive(Clone)]
pub(super) struct AsyncResumeHandle(pub(super) Rc<RefCell<AsyncFunctionData>>);

impl fmt::Debug for AsyncResumeHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AsyncResume")
    }
}

impl PartialEq for AsyncResumeHandle {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}
