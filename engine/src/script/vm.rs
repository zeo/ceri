// the bytecode interpreter: frames run in a loop without recursing on the Rust stack for
// JavaScript-to-JavaScript calls; generators and async functions suspend their frame and
// resume it later, and exceptions unwind through try handlers and finally blocks

use super::ast::{FunctionKind, Name};
use super::compiler::{self, BindingKind, Code, GLOBAL, Layout, NO_TARGET, Op, ScopeRole};
use super::object::{
    JsObject, ObjectClass, ObjectHandle, PRIVATE_PREFIX, Property, Slot, new_object,
};
use super::operations::{Callsite, type_name};
use super::realm::Native;
use super::*;

pub(super) use super::heap::ScopeRef;

pub(super) struct Scope {
    pub(super) slots: Vec<Value>,
    pub(super) parent: Option<ScopeRef>,
    pub(super) layout: Rc<Layout>,
    // names a direct eval added past the compiled layout, one per grown slot
    pub(super) extra: Grown,
}

// boxed and absent until an eval grows the scope, as nearly every scope never is
#[derive(Clone, Default)]
#[allow(clippy::box_collection)]
pub(super) struct Grown(Option<Box<Vec<Name>>>);

impl Grown {
    pub(super) fn push(&mut self, name: Name) {
        self.0.get_or_insert_with(Box::default).push(name);
    }
}

impl std::ops::Deref for Grown {
    type Target = [Name];

    fn deref(&self) -> &[Name] {
        self.0.as_deref().map_or(&[], Vec::as_slice)
    }
}

impl std::ops::DerefMut for Grown {
    fn deref_mut(&mut self) -> &mut [Name] {
        self.0.as_deref_mut().map_or(&mut [], Vec::as_mut_slice)
    }
}

impl Scope {
    #[allow(clippy::new_ret_no_self)]
    pub(super) fn new(layout: Rc<Layout>, parent: Option<ScopeRef>) -> ScopeRef {
        let slots = vec![Value::Undefined; layout.names.len()];
        Self::new_with_slots(layout, parent, slots)
    }

    pub(super) fn new_with_slots(
        layout: Rc<Layout>,
        parent: Option<ScopeRef>,
        slots: Vec<Value>,
    ) -> ScopeRef {
        super::heap::alloc_scope(Scope {
            slots,
            parent,
            layout,
            extra: Grown::default(),
        })
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

// the instance fields and private methods a class constructor installs on each instance,
// and the static ones it installs on itself
#[derive(Default)]
pub(super) struct ClassFields {
    pub(super) fields: Vec<FieldDefinition>,
    pub(super) statics: Vec<FieldDefinition>,
}

// rare class and method metadata allocated out of line so ordinary closures stay 24 bytes
#[derive(Default)]
pub(super) struct ClosureMeta {
    pub(super) home: Option<ObjectHandle>,
    // instance fields and private methods a class constructor installs on each instance
    pub(super) class: Option<ClassFields>,
}

pub(super) struct Closure {
    pub(super) code: Rc<Code>,
    pub(super) scope: Option<ScopeRef>,
    // null for ordinary functions and arrows; allocated on demand for methods and classes
    meta: std::cell::Cell<*mut ClosureMeta>,
}

impl Drop for Closure {
    fn drop(&mut self) {
        let ptr = *self.meta.get_mut();
        if !ptr.is_null() {
            // safety: non-null meta pointer came from Box::into_raw in meta_mut
            drop(unsafe { Box::from_raw(ptr) });
        }
    }
}

impl Closure {
    fn new(code: Rc<Code>, scope: Option<ScopeRef>) -> Self {
        Self {
            code,
            scope,
            meta: std::cell::Cell::new(std::ptr::null_mut()),
        }
    }

    fn with_meta_mut<R>(&self, body: impl FnOnce(&mut ClosureMeta) -> R) -> R {
        let mut ptr = self.meta.get();
        if ptr.is_null() {
            ptr = Box::into_raw(Box::default());
            self.meta.set(ptr);
        }
        // safety: single-threaded VM and callers never re-enter with_meta_mut
        body(unsafe { &mut *ptr })
    }

    pub(super) fn home(&self) -> Option<ObjectHandle> {
        let ptr = self.meta.get();
        if ptr.is_null() {
            return None;
        }
        // safety: non-null meta pointer is valid for self's lifetime
        unsafe { (*ptr).home.clone() }
    }

    pub(super) fn set_home_object(&self, home: ObjectHandle) {
        self.with_meta_mut(|meta| meta.home = Some(home));
    }

    pub(super) fn class_fields(&self) -> Vec<FieldDefinition> {
        let ptr = self.meta.get();
        if ptr.is_null() {
            return Vec::new();
        }
        // safety: non-null meta pointer is valid for self's lifetime
        unsafe { &*ptr }
            .class
            .as_ref()
            .map(|class| class.fields.clone())
            .unwrap_or_default()
    }

    pub(super) fn take_statics(&self) -> Vec<FieldDefinition> {
        let ptr = self.meta.get();
        if ptr.is_null() {
            return Vec::new();
        }
        // safety: single-threaded VM and no other reference to meta exists here
        unsafe { &mut *ptr }
            .class
            .as_mut()
            .map(|class| std::mem::take(&mut class.statics))
            .unwrap_or_default()
    }

    pub(super) fn add_class_field(&self, is_static: bool, definition: FieldDefinition) {
        self.with_meta_mut(|meta| {
            let class = meta.class.get_or_insert_with(Default::default);
            if is_static {
                class.statics.push(definition);
            } else {
                class.fields.push(definition);
            }
        });
    }

    pub(super) fn insert_private_method(&self, definition: FieldDefinition) {
        self.with_meta_mut(|meta| {
            let fields = &mut meta.class.get_or_insert_with(Default::default).fields;
            let position = fields
                .iter()
                .position(|field| matches!(field.kind, FieldKind::Field(_)))
                .unwrap_or(fields.len());
            fields.insert(position, definition);
        });
    }

    pub(super) fn trace_meta(self: &Rc<Self>, tracer: &mut super::heap::Tracer) {
        let ptr = self.meta.get();
        if ptr.is_null() {
            return;
        }
        // safety: GC runs with active_borrows == 0 and never mutates meta
        let meta = unsafe { &*ptr };
        if let Some(home) = &meta.home {
            tracer.mark_object(home.clone());
        }
        if let Some(class) = &meta.class {
            if !tracer.visit_rc(self) {
                return;
            }
            for field in class.fields.iter().chain(class.statics.iter()) {
                match &field.kind {
                    FieldKind::Field(Some(value))
                    | FieldKind::Method(value)
                    | FieldKind::Getter(value)
                    | FieldKind::Setter(value)
                    | FieldKind::StaticBlock(value) => tracer.trace_value(value),
                    FieldKind::Field(None) => {}
                }
            }
        }
    }
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

impl GeneratorState {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        if let Self::Suspended(frame) = self {
            frame.trace(tracer);
        }
    }
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

impl AsyncGeneratorData {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        self.state.trace(tracer);
        for request in &self.queue {
            tracer.trace_value(&request.value);
            tracer.trace_promise(&request.promise);
        }
    }
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

const FLAG_USES_REGS: u8 = 1 << 0;
const FLAG_OWNS_SCOPE: u8 = 1 << 1;
const FLAG_CONSTRUCT: u8 = 1 << 2;
const FLAG_EVAL: u8 = 1 << 3;
const FLAG_CALLEE_ON_STACK: u8 = 1 << 4;
// set whenever the frame's own scope could be referenced outside this frame
const FLAG_SCOPE_ESCAPED: u8 = 1 << 5;

// rare per-frame state kept out of line so ordinary calls push zero heap allocations
struct FrameExtra {
    new_target: Value,
    arguments: Vec<Value>,
    handlers: Vec<Handler>,
    pending: Vec<Pending>,
    chains: Vec<usize>,
    // a resume value injected into a suspended yield or await
    resume: Option<(ResumeMode, Value)>,
    // the outstanding resume mode for yield*, so DelegateStep forwards throw and return
    delegate_mode: ResumeMode,
    // the mode of the last async yield* call, which decides how its result unpacks
    delegate_last: ResumeMode,
    // operand values saved across generator/async suspension or initial non-simple args
    saved_stack: Vec<Value>,
    // arguments awaiting placement into registers on frame entry
    init_args: Vec<Value>,
}

impl Default for FrameExtra {
    fn default() -> Self {
        Self {
            new_target: Value::Undefined,
            arguments: Vec::new(),
            handlers: Vec::new(),
            pending: Vec::new(),
            chains: Vec::new(),
            resume: None,
            delegate_mode: ResumeMode::Next,
            delegate_last: ResumeMode::Next,
            saved_stack: Vec::new(),
            init_args: Vec::new(),
        }
    }
}

impl FrameExtra {
    fn is_empty(&self) -> bool {
        matches!(self.new_target, Value::Undefined)
            && self.arguments.is_empty()
            && self.handlers.is_empty()
            && self.pending.is_empty()
            && self.chains.is_empty()
            && self.resume.is_none()
            && self.delegate_mode == ResumeMode::Next
            && self.delegate_last == ResumeMode::Next
            && self.saved_stack.is_empty()
            && self.init_args.is_empty()
    }
}

pub(super) struct Frame {
    pub(super) code: Rc<Code>,
    closure: Option<Rc<Closure>>,
    pub(super) pc: usize,
    pub(super) reg_base: usize,
    stack_base: usize,
    return_base: usize,
    pub(super) scope: Option<ScopeRef>,
    pub(super) this: Value,
    function: Value,
    scopes: u16,
    flags: u8,
    extra: Option<Box<FrameExtra>>,
}

impl Frame {
    #[inline(always)]
    fn uses_regs(&self) -> bool {
        self.flags & FLAG_USES_REGS != 0
    }

    #[inline(always)]
    fn construct(&self) -> bool {
        self.flags & FLAG_CONSTRUCT != 0
    }

    #[inline(always)]
    fn is_eval(&self) -> bool {
        self.flags & FLAG_EVAL != 0
    }

    #[inline(always)]
    fn callee_on_stack(&self) -> bool {
        self.flags & FLAG_CALLEE_ON_STACK != 0
    }

    fn extra_mut(&mut self) -> &mut FrameExtra {
        self.extra.get_or_insert_with(Box::default)
    }

    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        if let Some(closure) = &self.closure {
            tracer.trace_closure(closure);
        }
        if let Some(scope) = &self.scope {
            tracer.mark_scope(scope.clone());
        }
        tracer.trace_value(&self.this);
        tracer.trace_value(&self.function);
        if let Some(extra) = &self.extra {
            tracer.trace_value(&extra.new_target);
            for value in &extra.arguments {
                tracer.trace_value(value);
            }
            for pending in &extra.pending {
                match pending {
                    Pending::Throw(value) | Pending::Return(value) => tracer.trace_value(value),
                    Pending::Normal | Pending::Jump { .. } => {}
                }
            }
            if let Some((_, value)) = &extra.resume {
                tracer.trace_value(value);
            }
            for value in &extra.saved_stack {
                tracer.trace_value(value);
            }
            for value in &extra.init_args {
                tracer.trace_value(value);
            }
        }
    }

    // whether a throw in this frame lands in one of its catch blocks
    pub(super) fn catches(&self) -> bool {
        self.extra.as_ref().is_some_and(|extra| {
            extra
                .handlers
                .iter()
                .any(|handler| handler.catch != NO_TARGET)
        })
    }

    fn function_value(&self, vm_stack: &[Value]) -> Value {
        if self.callee_on_stack() {
            vm_stack[self.reg_base - 1].clone()
        } else {
            self.function.clone()
        }
    }

    fn new_target_value(&self) -> Value {
        self.extra
            .as_ref()
            .map_or(Value::Undefined, |extra| extra.new_target.clone())
    }
}

#[inline(always)]
fn truncate_stack(stack: &mut Vec<Value>, new_len: usize) {
    while stack.len() > new_len {
        if let Some(value) = stack.pop() {
            value.fast_drop();
        }
    }
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
// the native stack the runs nested in one another may take from where the outermost
// began: a native callback re-enters run through tens of kilobytes of frames, a
// hundred in a debug build, and a script may run on a thread with as little as the
// 1 MiB a windows main thread gets
const NATIVE_STACK_BUDGET: usize = 640 * 1024;

impl ScriptRuntime {
    // ---- entry points ----

    pub(super) fn run_script_code(&mut self, code: Rc<Code>) -> Result<Value, ScriptError> {
        let _heap = super::heap::enter(self.heap_id);
        let scope = Scope::new(code.layout.clone(), None);
        let frame = Frame::new(
            code,
            None,
            Some(scope),
            Value::Window,
            Value::Undefined,
            Value::Undefined,
            Vec::new(),
        );
        let prev_gc = self.gc_safepoint_allowed;
        self.gc_safepoint_allowed = self.nested_runs == 0 && self.gc_inhibit == 0;
        let outcome = self.run(frame);
        self.gc_safepoint_allowed = prev_gc;
        match outcome? {
            RunResult::Return(value) => Ok(value),
            _ => Err(runtime_error("top-level script code cannot suspend")),
        }
    }

    // an indirect eval's code, run as global code
    pub(super) fn run_eval_code(
        &mut self,
        code: Rc<Code>,
        scope: ScopeRef,
    ) -> Result<Value, ScriptError> {
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
        code.set_url(&Rc::new(url));
        let closure = Rc::new(Closure::new(code.clone(), None));
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
        let prev_gc = self.gc_safepoint_allowed;
        self.gc_safepoint_allowed = self.nested_runs == 0 && self.gc_inhibit == 0;
        let outcome = self.run(frame);
        self.gc_safepoint_allowed = prev_gc;
        match outcome? {
            RunResult::Return(value) => Ok(value),
            RunResult::Await(value, frame) => {
                let promise = new_promise(PromiseOutcome::Pending);
                let state = Rc::new(RefCell::new(AsyncFunctionData {
                    frame: Some(frame),
                    promise: promise.clone(),
                    generator: None,
                }));
                self.await_then_resume(value, state)?;
                let root_base = self.temp_roots.len();
                self.temp_roots.push(Value::Promise(promise.clone()));
                let drained = self.drain_microtasks();
                self.temp_roots.truncate(root_base);
                drained?;
                Ok(Value::Promise(promise))
            }
            RunResult::Yield(..) => Err(runtime_error("modules cannot yield")),
        }
    }

    pub(super) fn new_closure(&self, code: Rc<Code>, scope: Option<ScopeRef>) -> Value {
        let closure = Rc::new(Closure::new(code, scope));
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
        let constructible = matches!(
            code.kind,
            FunctionKind::Normal
                | FunctionKind::ClassConstructor
                | FunctionKind::DerivedConstructor
        );
        let own_prototype = code.is_generator || (constructible && !code.is_async);
        // the closures of one arrow or method share their length and name until one changes
        if !own_prototype && let Some(template) = code.function_template.borrow().as_ref() {
            return Value::Object(new_object(JsObject::from_template(
                template,
                Some(prototype),
                ObjectClass::Function(closure),
            )));
        }
        let mut object = JsObject::with_prototype(Some(prototype), ObjectClass::Function(closure));
        // length, name, and a prototype for what can be constructed or is a generator
        object.reserve(2 + usize::from(own_prototype));
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
        if !own_prototype {
            *code.function_template.borrow_mut() = object.template();
        }
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
        let _heap = super::heap::enter(self.heap_id);
        match self.prepare_call(callee, this, arguments, None)? {
            Prepared::Done(value) => Ok(value),
            Prepared::Frame(frame) => self.run_to_value(frame),
        }
    }

    pub(super) fn construct(
        &mut self,
        callee: Value,
        arguments: Vec<Value>,
        new_target: Option<Value>,
    ) -> Result<Value, ScriptError> {
        let _heap = super::heap::enter(self.heap_id);
        let new_target = new_target.unwrap_or_else(|| callee.clone());
        match self.prepare_construct(callee, arguments, new_target)? {
            Prepared::Done(value) => Ok(value),
            Prepared::Frame(frame) => self.run_to_value(frame),
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
                    return Ok(Prepared::Frame(frame));
                }
                ObjectClass::Bound(bound) => {
                    let super::object::BoundFunction {
                        target,
                        this,
                        arguments: bound,
                    } = *bound;
                    let mut all = bound;
                    all.extend(arguments);
                    return self.prepare_call(target, this, all, None);
                }
                ObjectClass::Native(native) => match native.as_ref() {
                    // keep Function.prototype.call and apply on the bytecode frame stack so
                    // they avoid Rust stack recursion and preserve safepoint GC eligibility
                    Native::FunctionCall => {
                        if !is_callable(&this) {
                            return Err(
                                self.type_error("Function.prototype.call called on non-function")
                            );
                        }
                        let mut arguments = arguments.into_iter();
                        let receiver = arguments.next().unwrap_or(Value::Undefined);
                        return self.prepare_call(this, receiver, arguments.collect(), None);
                    }
                    Native::FunctionApply => {
                        if !is_callable(&this) {
                            return Err(self.type_error(
                                "Function.prototype.apply was called on a non-function",
                            ));
                        }
                        let receiver = arguments.first().cloned().unwrap_or(Value::Undefined);
                        let list =
                            self.array_like_values(arguments.get(1).unwrap_or(&Value::Undefined))?;
                        return self.prepare_call(this, receiver, list, None);
                    }
                    Native::Reflect(super::reflect::ReflectMethod::Apply) => {
                        let target = arguments.first().cloned().unwrap_or(Value::Undefined);
                        if !is_callable(&target) {
                            return Err(self.type_error("Reflect.apply target is not a function"));
                        }
                        let receiver = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                        let list =
                            self.array_like_list(arguments.get(2).unwrap_or(&Value::Undefined))?;
                        return self.prepare_call(target, receiver, list, None);
                    }
                    Native::Reflect(super::reflect::ReflectMethod::Get) => {
                        let target = arguments.first().cloned().unwrap_or(Value::Undefined);
                        if super::operations::is_primitive(&target) {
                            return Err(self.type_error("Reflect target must be an object"));
                        }
                        let key =
                            self.coerce_key(arguments.get(1).cloned().unwrap_or(Value::Undefined))?;
                        let receiver = if arguments.len() > 2 {
                            arguments[2].clone()
                        } else {
                            target.clone()
                        };
                        let outcome = match &target {
                            Value::Object(object) => {
                                self.ordinary_get_tail(object.clone(), &key, &receiver)?
                            }
                            other => self.get_property_tail(other, &key)?,
                        };
                        return match outcome {
                            super::operations::GetOutcome::Value(value) => {
                                Ok(Prepared::Done(value))
                            }
                            super::operations::GetOutcome::Call {
                                callee,
                                this,
                                arguments,
                            } => self.prepare_call(callee, this, arguments, None),
                        };
                    }
                    _ => {
                        return Ok(Prepared::Done(self.call_native(
                            Rc::unwrap_or_clone(native),
                            this,
                            arguments,
                        )?));
                    }
                },
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
                            self.prepare_call(trap, handler, vec![target, this, list], None)
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
                    return Ok(Prepared::Frame(frame));
                }
                ObjectClass::Bound(bound) => {
                    let super::object::BoundFunction {
                        target,
                        arguments: bound,
                        ..
                    } = *bound;
                    let mut all = bound;
                    all.extend(arguments);
                    let new_target = if ObjectHandle::ptr_eq(
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
                    return Ok(Prepared::Done(self.construct_native(
                        Rc::unwrap_or_clone(native),
                        arguments,
                        new_target,
                    )?));
                }
                ObjectClass::Proxy(proxy) => {
                    if !proxy.constructor {
                        return Err(self.type_error("value is not a constructor"));
                    }
                    let (target, trap) = self.proxy_trap(&proxy, "construct")?;
                    let new_target = match &new_target {
                        Value::Object(target_object)
                            if ObjectHandle::ptr_eq(target_object, object) =>
                        {
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
        let fields = closure.class_fields();
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
        let (mut get, mut set) = match object.property(key).map(|property| property.slot) {
            Some(Slot::Accessor(accessors)) => (accessors.get.clone(), accessors.set.clone()),
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
                slot: Slot::accessor(get, set),
                enumerable,
                writable: false,
                configurable: true,
            },
        );
    }

    fn new_scope(&mut self, layout: Rc<Layout>, parent: Option<ScopeRef>) -> ScopeRef {
        Scope::new(layout, parent)
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
        if code.uses_regs {
            let mut frame = Frame::new(
                code,
                Some(closure.clone()),
                closure.scope.clone(),
                this,
                new_target,
                function.clone(),
                Vec::new(),
            );
            frame.flags |= FLAG_USES_REGS;
            if construct {
                frame.flags |= FLAG_CONSTRUCT;
            }
            if !arguments.is_empty() {
                frame.extra_mut().init_args = arguments;
            }
            return frame;
        }
        let parent = if let Some(layout) = &code.self_scope {
            let scope = self.new_scope(layout.clone(), closure.scope.clone());
            scope.borrow_mut().slots[0] = function.clone();
            Some(scope)
        } else {
            closure.scope.clone()
        };
        let scope = self.new_scope(code.layout.clone(), parent);
        let simple = code.simple_params;
        let moves = simple && !code.reads_arguments;
        let mut frame = Frame::new(
            code.clone(),
            Some(closure.clone()),
            Some(scope.clone()),
            this,
            new_target,
            function.clone(),
            if moves { Vec::new() } else { arguments.clone() },
        );
        frame.flags |= FLAG_OWNS_SCOPE;
        if construct {
            frame.flags |= FLAG_CONSTRUCT;
        }
        if simple {
            let mut scope = scope.borrow_mut();
            let mut args = arguments;
            for (position, slot) in code.param_slots.iter().enumerate() {
                if let Some(slot) = slot {
                    scope.slots[*slot as usize] = match args.get_mut(position) {
                        Some(value) if moves => std::mem::replace(value, Value::Undefined),
                        Some(value) => value.clone(),
                        None => Value::Undefined,
                    };
                }
            }
        } else {
            let arguments = Self::new_array(arguments);
            frame.extra_mut().saved_stack.push(arguments);
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
            Value::Builtin(Builtin::Native(native)) => {
                self.call_native(Rc::unwrap_or_clone(native), this, arguments)
            }
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
        // AggregateError(errors, message, options) and SuppressedError(error, suppressed, message)
        // take their message after the values they carry
        let message_at = match kind {
            "AggregateError" => 1,
            "SuppressedError" => 2,
            _ => 0,
        };
        if let Some(message) = arguments
            .get(message_at)
            .filter(|value| !matches!(value, Value::Undefined))
        {
            let message = self.coerce_string(message.clone())?;
            object.define(
                "message".to_string(),
                Property::hidden(Value::String(message.into())),
            );
        }
        if kind != "SuppressedError"
            && let Some(options @ Value::Object(_)) = arguments.get(message_at + 1)
            && self.has_property(options, "cause")?
        {
            let cause = self.get_property(options, "cause")?;
            object.define("cause".to_string(), Property::hidden(cause));
        }
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        match kind {
            "AggregateError" => {
                let errors = self.iterate_to_vec(argument(0))?;
                object.define(
                    "errors".to_string(),
                    Property::hidden(Self::new_array(errors)),
                );
            }
            "SuppressedError" => {
                object.define("error".to_string(), Property::hidden(argument(0)));
                object.define("suppressed".to_string(), Property::hidden(argument(1)));
            }
            _ => {}
        }
        Ok(self.with_stack(object))
    }

    // ---- the run loop ----

    fn push_frame(&mut self, mut frame: Frame) {
        let base = self.vm_stack.len();
        frame.return_base = base;
        frame.reg_base = base;
        if frame.uses_regs() {
            let reg_count = frame.code.registers as usize;
            self.vm_stack.resize(base + reg_count, Value::Undefined);
            if let Some(extra) = frame.extra.as_mut() {
                let mut args = std::mem::take(&mut extra.init_args);
                for (position, slot) in frame.code.param_slots.iter().enumerate() {
                    if let Some(slot) = slot {
                        self.vm_stack[base + *slot as usize] = match args.get_mut(position) {
                            Some(value) => std::mem::replace(value, Value::Undefined),
                            None => Value::Undefined,
                        };
                    }
                }
                if extra.is_empty() {
                    frame.extra = None;
                }
            }
            frame.stack_base = base + reg_count;
        } else {
            frame.stack_base = base;
            if let Some(extra) = frame.extra.as_mut()
                && !extra.saved_stack.is_empty()
            {
                self.vm_stack.append(&mut extra.saved_stack);
            }
        }
        self.frames.push(frame);
    }

    pub(super) fn run(&mut self, frame: Frame) -> Result<RunResult, ScriptError> {
        let _heap = super::heap::enter(self.heap_id);
        let marker = 0u8;
        let here = std::hint::black_box(&marker) as *const u8 as usize;
        if self.nested_runs == 0 {
            self.stack_base = here;
        } else if self.nested_runs >= MAX_NESTED_RUNS
            || self.stack_base.saturating_sub(here) > NATIVE_STACK_BUDGET
        {
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        self.nested_runs += 1;
        let mirror_base = self.debugger.mirror.len();
        let frame_floor = self.frames.len();
        let stack_floor = self.vm_stack.len();
        self.push_frame(frame);
        let result = self.run_frames(frame_floor);
        if result.is_err() {
            self.frames.truncate(frame_floor);
            truncate_stack(&mut self.vm_stack, stack_floor);
        }
        debug_assert_eq!(self.frames.len(), frame_floor);
        debug_assert_eq!(self.vm_stack.len(), stack_floor);
        self.debugger.mirror.truncate(mirror_base);
        self.nested_runs -= 1;
        if self.nested_runs == 0 {
            self.debug_task_ended();
        }
        result
    }

    fn safepoint_target(&self, sampling: bool) -> u64 {
        if self.interrupted {
            return 0;
        }
        let mut target = self
            .step_limit
            .map_or(u64::MAX, |limit| limit.saturating_add(1));
        if sampling {
            target = target.min(self.steps.saturating_add(1024));
        } else if self.gc_safepoint_allowed && self.nested_runs == 1 {
            target = target.min(self.steps.saturating_add(32_768));
        }
        target
    }

    #[cold]
    #[inline(never)]
    fn hit_safepoint(&mut self, sampling: bool, pc: usize) -> Result<u64, ScriptError> {
        if self.step_limit.is_some_and(|limit| self.steps > limit) {
            self.interrupted = true;
        }
        if self.interrupted {
            return Err(super::runtime_error("script exceeded its step limit"));
        }
        if sampling {
            if let Some(top) = self.frames.last_mut() {
                top.pc = pc;
            }
            self.sample_script();
        }
        self.collect_if_due();
        Ok(self.safepoint_target(sampling))
    }

    fn run_frames(&mut self, frame_floor: usize) -> Result<RunResult, ScriptError> {
        let sampling = crate::profile::enabled();
        loop {
            let outcome = if self.debugger.active {
                self.run_debug_step(frame_floor, sampling)
            } else {
                self.run_fast(frame_floor, sampling).map(Some)
            };
            match outcome {
                Ok(Some(result)) => return Ok(result),
                Ok(None) => {}
                Err(error) if self.interrupted => return Err(error),
                Err(error) => {
                    let thrown = self.take_thrown(error);
                    self.note_thrown(&thrown);
                    if self.debugger.active {
                        self.debug_thrown(&thrown);
                    }
                    if let Some(result) = self.unwind(frame_floor, thrown)? {
                        return Ok(result);
                    }
                }
            }
        }
    }

    fn run_debug_step(
        &mut self,
        frame_floor: usize,
        sampling: bool,
    ) -> Result<Option<RunResult>, ScriptError> {
        self.steps += 1;
        if sampling && self.steps.is_multiple_of(1024) {
            self.sample_script();
        }
        if self.step_limit.is_some_and(|limit| self.steps > limit) {
            self.interrupted = true;
        }
        if self.interrupted {
            return Err(super::runtime_error("script exceeded its step limit"));
        }
        self.debug_step();
        let top = self.frames.len() - 1;
        let pc = self.frames[top].pc;
        let op = self.frames[top].code.ops[pc];
        self.frames[top].pc = pc + 1;
        self.step_op(op, frame_floor)
    }

    // notes the functions running for the profile, each by its name and the line it starts on
    fn sample_script(&mut self) {
        thread_local! {
            static LABELS: RefCell<HashMap<usize, Rc<str>>> = RefCell::new(HashMap::default());
        }
        let label = |code: &Rc<Code>| {
            LABELS.with(|labels| {
                labels
                    .borrow_mut()
                    .entry(Rc::as_ptr(code) as usize)
                    .or_insert_with(|| {
                        let start = code.source.as_ref().map_or(0, |(_, start, _)| *start);
                        let before = code.text.get(..start).unwrap_or_default();
                        let line = before.matches('\n').count() + 1;
                        let column = before.len() - before.rfind('\n').map_or(0, |at| at + 1) + 1;
                        let name = if code.name.is_empty() {
                            "(anonymous)"
                        } else {
                            &code.name
                        };
                        format!("{name} {line}:{column}").into()
                    })
                    .clone()
            })
        };
        let stack: Vec<Rc<str>> = self.frames.iter().map(|frame| label(&frame.code)).collect();
        crate::profile::sample(&stack);
    }

    // throws a value inside the frame stack: the nearest handler takes it, or the run ends
    fn unwind(
        &mut self,
        frame_floor: usize,
        thrown: Value,
    ) -> Result<Option<RunResult>, ScriptError> {
        loop {
            if self.frames.len() <= frame_floor {
                return Err(self.throw_value(thrown));
            }
            let top = self.frames.len() - 1;
            let handler = self.frames[top]
                .extra
                .as_mut()
                .and_then(|extra| extra.handlers.pop());
            if let Some(handler) = handler {
                let target_stack = self.frames[top].stack_base + handler.stack;
                truncate_stack(&mut self.vm_stack, target_stack);
                let frame = &mut self.frames[top];
                Self::restore_scopes(frame, handler.scopes);
                if let Some(extra) = frame.extra.as_mut() {
                    extra.chains.truncate(handler.chains);
                }
                if handler.catch != NO_TARGET {
                    self.vm_stack.push(thrown);
                    frame.pc = handler.catch as usize;
                    self.note_caught();
                } else {
                    frame.extra_mut().pending.push(Pending::Throw(thrown));
                    frame.pc = handler.finally as usize;
                }
                return Ok(None);
            }
            if self.frames.len() == frame_floor + 1 {
                return Err(self.throw_value(thrown));
            }
            let finished = self.frames.pop().expect("frame");
            truncate_stack(&mut self.vm_stack, finished.return_base);
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

    fn with_scope_at<R>(frame: &Frame, depth: u16, read: impl FnOnce(&Scope) -> R) -> R {
        read(&Self::scope_at(frame, depth).borrow())
    }

    #[inline]
    fn scope_at(frame: &Frame, depth: u16) -> ScopeRef {
        let mut scope = frame.scope.clone().expect("a scope is present");
        for _ in 0..depth {
            let parent = scope.borrow().parent.clone().expect("scope depth is valid");
            scope = parent;
        }
        scope
    }

    // what answers for a name before its fallback binding: a with object that has
    // it, or a binding a direct eval grew, on the scopes nearer than depth
    fn shadowing(
        &mut self,
        scope: Option<ScopeRef>,
        name: &str,
        depth: u16,
    ) -> Result<Option<Shadow>, ScriptError> {
        let mut walk = scope;
        let mut level = 0;
        while let Some(current) = walk {
            if depth != GLOBAL && level >= depth {
                break;
            }
            let (object, grown, parent) = {
                let scope = current.borrow();
                (
                    (scope.layout.role == ScopeRole::With).then(|| scope.slots[0].clone()),
                    scope
                        .extra
                        .iter()
                        .position(|extra| &**extra == name)
                        .map(|position| scope.layout.names.len() + position),
                    scope.parent.clone(),
                )
            };
            if let Some(object) = object {
                if self.with_has(&object, name)? {
                    return Ok(Some(Shadow::With(object)));
                }
            } else if let Some(index) = grown {
                return Ok(Some(Shadow::Grown(current, index)));
            }
            walk = parent;
            level += 1;
        }
        Ok(None)
    }

    // whether a with object answers for a name its @@unscopables leaves alone
    fn with_has(&mut self, object: &Value, key: &str) -> Result<bool, ScriptError> {
        if !self.has_property(object, key)? {
            return Ok(false);
        }
        let unscopables = self.get_property(object, "@@unscopables")?;
        if super::operations::is_primitive(&unscopables) {
            return Ok(true);
        }
        Ok(!self.get_property(&unscopables, key)?.truthy())
    }

    // a compiled binding's value, which throws in its dead zone
    fn read_local(
        &mut self,
        top: usize,
        depth: u16,
        index: u16,
        name: u32,
    ) -> Result<Value, ScriptError> {
        let value = Self::with_scope_at(&self.frames[top], depth, |scope| {
            match &scope.slots[index as usize] {
                Value::Binding(cell) | Value::ConstBinding(cell) => cell.borrow().clone(),
                value => value.clone(),
            }
        });
        if matches!(value, Value::Uninitialized) {
            let name = self.frames[top].code.names[name as usize].clone();
            return Err(self.reference_error(format!("cannot access {name} before initialization")));
        }
        Ok(value)
    }

    fn write_local(
        &mut self,
        top: usize,
        depth: u16,
        index: u16,
        name: u32,
        value: Value,
    ) -> Result<(), ScriptError> {
        let scope_ref = Self::scope_at(&self.frames[top], depth);
        let mut scope = scope_ref.borrow_mut();
        match &mut scope.slots[index as usize] {
            Value::Binding(cell) if !matches!(*cell.borrow(), Value::Uninitialized) => {
                *cell.borrow_mut() = value;
            }
            Value::ConstBinding(_) => {
                drop(scope);
                let name = self.frames[top].code.names[name as usize].clone();
                return Err(self.type_error(format!("Assignment to constant variable '{name}'")));
            }
            Value::Binding(_) | Value::Uninitialized => {
                drop(scope);
                let name = self.frames[top].code.names[name as usize].clone();
                return Err(
                    self.reference_error(format!("cannot access {name} before initialization"))
                );
            }
            slot => *slot = value,
        }
        Ok(())
    }

    // a name op's value when nothing shadows the name
    fn read_fallback(
        &mut self,
        top: usize,
        name: u32,
        depth: u16,
        index: u16,
    ) -> Result<Value, ScriptError> {
        if depth == GLOBAL {
            let key = self.frames[top].code.names[name as usize].clone();
            return self.get_global(&key);
        }
        self.read_local(top, depth, index, name)
    }

    fn read_name(
        &mut self,
        top: usize,
        name: u32,
        depth: u16,
        index: u16,
    ) -> Result<(Value, Value), ScriptError> {
        let key = self.frames[top].code.names[name as usize].clone();
        let scope = self.frames[top].scope.clone();
        Ok(match self.shadowing(scope, &key, depth)? {
            Some(Shadow::With(object)) => {
                let value = self.get_property(&object, &key)?;
                (object, value)
            }
            Some(Shadow::Grown(scope, index)) => {
                let value = scope.borrow().slots[index].clone();
                (Value::Undefined, value)
            }
            None => (
                Value::Undefined,
                self.read_fallback(top, name, depth, index)?,
            ),
        })
    }

    // the this and new.target of the function an arrow's code sits in, which it keeps
    // in its hidden slots there
    fn lexical_value(frame: &Frame, hidden: &str) -> Option<Value> {
        let mut walk = frame.scope.clone();
        while let Some(scope) = walk {
            let found = {
                let scope = scope.borrow();
                scope
                    .layout
                    .names
                    .iter()
                    .position(|name| &**name == hidden)
                    .map(|index| scope.slots[index].clone())
            };
            if found.is_some() {
                return found;
            }
            walk = scope.borrow().parent.clone();
        }
        None
    }

    // a direct eval runs in a frame of its own over the caller's scopes, with the
    // caller's this, new.target, function and home object
    fn direct_eval(&mut self, source: &str) -> Result<Option<RunResult>, ScriptError> {
        let frame = self.frames.last_mut().expect("a frame is running");
        frame.flags |= FLAG_SCOPE_ESCAPED;
        let caller_url = frame.code.url.get().cloned();
        let (this, new_target) = if frame.code.kind == FunctionKind::Arrow {
            (
                Self::lexical_value(frame, "%this").unwrap_or(Value::Window),
                Self::lexical_value(frame, "%newtarget").unwrap_or(Value::Undefined),
            )
        } else {
            (frame.this.clone(), frame.new_target_value())
        };
        let closure = frame.closure.clone();
        let function = frame.function_value(&self.vm_stack);
        let (strict, context) = (frame.code.strict, frame.code.eval_context);
        let scope = frame.scope.clone();
        let (code, scope) = self.instantiate_eval(source, strict, context, scope)?;
        if let Some(url) = caller_url {
            code.set_url(&url);
        }
        if self.frames.len() >= MAX_FRAMES {
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        let mut frame = Frame::new(
            code,
            closure,
            Some(scope),
            this,
            new_target,
            function,
            Vec::new(),
        );
        frame.flags |= FLAG_EVAL | FLAG_OWNS_SCOPE;
        self.push_frame(frame);
        Ok(None)
    }

    // returns from the top frame, running finally blocks on the way out
    fn do_return(
        &mut self,
        frame_floor: usize,
        value: Value,
    ) -> Result<Option<RunResult>, ScriptError> {
        let top = self.frames.len() - 1;
        while let Some(handler) = self.frames[top]
            .extra
            .as_mut()
            .and_then(|extra| extra.handlers.pop())
        {
            if handler.finally != NO_TARGET {
                let target_stack = self.frames[top].stack_base + handler.stack;
                truncate_stack(&mut self.vm_stack, target_stack);
                let frame = &mut self.frames[top];
                Self::restore_scopes(frame, handler.scopes);
                if let Some(extra) = frame.extra.as_mut() {
                    extra.chains.truncate(handler.chains);
                }
                frame.extra_mut().pending.push(Pending::Return(value));
                frame.pc = handler.finally as usize;
                return Ok(None);
            }
        }
        let mut finished = self.frames.pop().expect("a frame is running");
        truncate_stack(&mut self.vm_stack, finished.return_base);
        let value = if finished.construct() {
            match value {
                // host objects such as nodes count as objects, so a custom element's
                // constructor can hand back the element super() produced
                ref object if !super::operations::is_primitive(object) => value,
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
                    std::mem::replace(&mut finished.this, Value::Undefined)
                }
            }
        } else {
            value
        };
        if self.frames.len() > frame_floor {
            self.vm_stack.push(value);
            Ok(None)
        } else {
            Ok(Some(RunResult::Return(value)))
        }
    }

    // the ordinary call, done in place on the shared stack: register-only functions reuse
    // their caller's argument slots as registers with zero scope or frame allocation; false
    // leaves the stack unchanged for the general call path
    fn call_plain(&mut self, count: usize, method: bool) -> bool {
        if self.frames.len() >= MAX_FRAMES {
            return false;
        }
        let arg_start = match self.vm_stack.len().checked_sub(count) {
            Some(start) => start,
            None => return false,
        };
        let Some(callee_at) = arg_start.checked_sub(1) else {
            return false;
        };
        let Some(return_base) = callee_at.checked_sub(usize::from(method)) else {
            return false;
        };
        let closure = {
            let Value::Object(object) = &self.vm_stack[callee_at] else {
                return false;
            };
            let Ok(object) = object.try_borrow() else {
                return false;
            };
            let ObjectClass::Function(closure) = &object.class else {
                return false;
            };
            closure.clone()
        };
        let code = closure.code.clone();
        if !code.simple_params
            || code.reads_arguments
            || code.self_scope.is_some()
            || code.is_generator
            || code.is_async
            || matches!(
                code.kind,
                FunctionKind::ClassConstructor | FunctionKind::DerivedConstructor
            )
        {
            return false;
        }
        let this = if method {
            std::mem::replace(&mut self.vm_stack[return_base], Value::Undefined)
        } else {
            Value::Undefined
        };
        let this = if code.strict || code.kind == FunctionKind::Arrow {
            this
        } else if matches!(this, Value::Undefined | Value::Null) {
            Value::Window
        } else {
            this
        };
        if code.uses_regs {
            let reg_count = code.registers as usize;
            let param_count = code.param_slots.len();
            if code.linear_params {
                let keep = count.min(param_count);
                if count > keep {
                    truncate_stack(&mut self.vm_stack, arg_start + keep);
                }
                if keep < reg_count {
                    self.vm_stack
                        .resize(arg_start + reg_count, Value::Undefined);
                }
            } else {
                // duplicate parameter names in sloppy mode: last occurrence wins
                let mut args = self.vm_stack.split_off(arg_start);
                self.vm_stack
                    .resize(arg_start + reg_count, Value::Undefined);
                for (position, slot) in code.param_slots.iter().enumerate() {
                    if let Some(slot) = slot {
                        self.vm_stack[arg_start + *slot as usize] = match args.get_mut(position) {
                            Some(value) => std::mem::replace(value, Value::Undefined),
                            None => Value::Undefined,
                        };
                    }
                }
            }
            let pc = code.entry;
            let scope = closure.scope.clone();
            self.frames.push(Frame {
                code,
                closure: Some(closure),
                pc,
                reg_base: arg_start,
                stack_base: arg_start + reg_count,
                return_base,
                scope,
                this,
                function: Value::Undefined,
                scopes: 0,
                flags: FLAG_USES_REGS | FLAG_CALLEE_ON_STACK,
                extra: None,
            });
            return true;
        }
        let scope = self.new_scope(code.layout.clone(), closure.scope.clone());
        {
            let mut slots = scope.borrow_mut();
            // every parameter is written, the ones past the arguments as undefined, so the
            // last of two parameters with one name decides its value
            for (position, slot) in code.param_slots.iter().enumerate() {
                if let Some(slot) = slot {
                    slots.slots[*slot as usize] = if position < count {
                        std::mem::replace(
                            &mut self.vm_stack[arg_start + position],
                            Value::Undefined,
                        )
                    } else {
                        Value::Undefined
                    };
                }
            }
        }
        truncate_stack(&mut self.vm_stack, arg_start);
        let pc = code.entry;
        self.frames.push(Frame {
            code,
            closure: Some(closure),
            pc,
            reg_base: arg_start,
            stack_base: arg_start,
            return_base,
            scope: Some(scope),
            this,
            function: Value::Undefined,
            scopes: 0,
            flags: FLAG_OWNS_SCOPE | FLAG_CALLEE_ON_STACK,
            extra: None,
        });
        true
    }

    // leaves try regions down to a depth, running each finally block before the jump lands
    fn do_exit(&mut self, target: u32, handlers: u16, scopes: u16) {
        let top = self.frames.len() - 1;
        while self.frames[top]
            .extra
            .as_ref()
            .is_some_and(|extra| extra.handlers.len() > handlers as usize)
        {
            let handler = self.frames[top]
                .extra
                .as_mut()
                .expect("checked above")
                .handlers
                .pop()
                .expect("checked above");
            if handler.finally != NO_TARGET {
                let target_stack = self.frames[top].stack_base + handler.stack;
                truncate_stack(&mut self.vm_stack, target_stack);
                let frame = &mut self.frames[top];
                Self::restore_scopes(frame, handler.scopes);
                if let Some(extra) = frame.extra.as_mut() {
                    extra.chains.truncate(handler.chains);
                }
                frame.extra_mut().pending.push(Pending::Jump {
                    target,
                    handlers,
                    scopes,
                });
                frame.pc = handler.finally as usize;
                return;
            }
        }
        let frame = &mut self.frames[top];
        Self::restore_scopes(frame, scopes);
        frame.pc = target as usize;
    }

    #[inline(always)]
    fn pop(&mut self) -> Value {
        self.vm_stack.pop().unwrap_or(Value::Undefined)
    }

    fn pop_arguments(&mut self, count: usize) -> Vec<Value> {
        let start = self.vm_stack.len().saturating_sub(count);
        self.vm_stack.split_off(start)
    }

    // unified hot interpreter loop: keeps the active frame's code, pc and register base in
    // locals, stays inline across calls and returns, and checks safepoints only at backward
    // jumps and calls
    fn run_fast(&mut self, frame_floor: usize, sampling: bool) -> Result<RunResult, ScriptError> {
        let mut top = self.frames.len() - 1;
        let mut code: *const Code = Rc::as_ptr(&self.frames[top].code);
        let mut pc: usize = self.frames[top].pc;
        let mut reg_base: usize = self.frames[top].reg_base;
        let mut block_start: usize = pc;
        let mut safepoint_at: u64 = self.safepoint_target(sampling);
        loop {
            // safety: self.frames[top] owns Rc<Code> until frames changes, at which point we reload
            let op_pc = pc;
            let op = unsafe { (&*code).ops[op_pc] };
            pc = op_pc + 1;
            match op {
                Op::Nop => {}
                Op::Undefined => self.vm_stack.push(Value::Undefined),
                Op::Null => self.vm_stack.push(Value::Null),
                Op::True => self.vm_stack.push(Value::Bool(true)),
                Op::False => self.vm_stack.push(Value::Bool(false)),
                Op::Number(value) => self.vm_stack.push(Value::Number(value)),
                Op::String(index) => {
                    let text = unsafe { (&*code).names[index as usize].clone() };
                    self.vm_stack.push(Value::String(text));
                }
                Op::Pop => {
                    if let Some(value) = self.vm_stack.pop() {
                        value.fast_drop();
                    }
                }
                Op::Dup => {
                    let copy = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                    self.vm_stack.push(copy);
                }
                Op::Swap => {
                    let length = self.vm_stack.len();
                    self.vm_stack.swap(length - 1, length - 2);
                }
                Op::GetReg { reg, name } => {
                    let value = &self.vm_stack[reg_base + reg as usize];
                    if matches!(value, Value::Uninitialized) {
                        self.frames[top].pc = pc;
                        let name = unsafe { (&*code).names[name as usize].clone() };
                        return Err(self.reference_error(format!(
                            "cannot access {name} before initialization"
                        )));
                    }
                    let copy = value.clone();
                    self.vm_stack.push(copy);
                }
                Op::SetReg { reg, name } => {
                    let slot_index = reg_base + reg as usize;
                    if matches!(self.vm_stack[slot_index], Value::Uninitialized) {
                        self.frames[top].pc = pc;
                        let name = unsafe { (&*code).names[name as usize].clone() };
                        return Err(self.reference_error(format!(
                            "cannot access {name} before initialization"
                        )));
                    }
                    let value = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                    let old = std::mem::replace(&mut self.vm_stack[slot_index], value);
                    old.fast_drop();
                }
                Op::InitReg(reg) => {
                    let value = self.vm_stack.pop().unwrap_or(Value::Undefined);
                    let old = std::mem::replace(&mut self.vm_stack[reg_base + reg as usize], value);
                    old.fast_drop();
                }
                Op::UpdateReg {
                    reg,
                    name,
                    increment,
                    prefix,
                } => {
                    let slot_index = reg_base + reg as usize;
                    if let Value::Number(slot) = &mut self.vm_stack[slot_index] {
                        let old = *slot;
                        let new = if increment { old + 1.0 } else { old - 1.0 };
                        *slot = new;
                        self.vm_stack
                            .push(Value::Number(if prefix { new } else { old }));
                    } else {
                        self.frames[top].pc = pc;
                        self.update_reg_slow(top, reg, name, increment, prefix)?;
                    }
                }
                Op::UninitRegs { index, count } => {
                    let start = reg_base + index as usize;
                    let end = start + count as usize;
                    for slot in &mut self.vm_stack[start..end] {
                        let old = std::mem::replace(slot, Value::Uninitialized);
                        old.fast_drop();
                    }
                }
                Op::GetLocal { depth, index, name } => {
                    let scope_ref = Self::scope_at(&self.frames[top], depth);
                    let fast = scope_ref.try_borrow().ok().and_then(|scope| {
                        match scope.slots.get(index as usize) {
                            Some(
                                Value::Binding(_) | Value::ConstBinding(_) | Value::Uninitialized,
                            )
                            | None => None,
                            Some(value) => Some(value.clone()),
                        }
                    });
                    match fast {
                        Some(value) => self.vm_stack.push(value),
                        None => {
                            self.frames[top].pc = pc;
                            let value = self.read_local(top, depth, index, name)?;
                            self.vm_stack.push(value);
                        }
                    }
                }
                Op::SetLocal { depth, index, name } => {
                    let scope_ref = Self::scope_at(&self.frames[top], depth);
                    let written = scope_ref.try_borrow_mut().ok().is_some_and(|mut scope| {
                        match scope.slots.get_mut(index as usize) {
                            Some(
                                Value::Binding(_) | Value::ConstBinding(_) | Value::Uninitialized,
                            )
                            | None => false,
                            Some(slot) => {
                                let value =
                                    self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                                let old = std::mem::replace(slot, value);
                                old.fast_drop();
                                true
                            }
                        }
                    });
                    if !written {
                        self.frames[top].pc = pc;
                        let value = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                        self.write_local(top, depth, index, name, value)?;
                    }
                }
                Op::InitLocal { depth, index } => {
                    let value = self.pop();
                    let scope_ref = Self::scope_at(&self.frames[top], depth);
                    let mut scope = scope_ref.borrow_mut();
                    match &scope.slots[index as usize] {
                        Value::Binding(cell) | Value::ConstBinding(cell) => {
                            let old = std::mem::replace(&mut *cell.borrow_mut(), value);
                            old.fast_drop();
                        }
                        _ => {
                            let old = std::mem::replace(&mut scope.slots[index as usize], value);
                            old.fast_drop();
                        }
                    }
                }
                Op::UpdateLocal {
                    depth,
                    index,
                    name,
                    increment,
                    prefix,
                } => {
                    let scope_ref = Self::scope_at(&self.frames[top], depth);
                    let fast = scope_ref.try_borrow_mut().ok().and_then(|mut scope| {
                        let Value::Number(slot) = scope.slots.get_mut(index as usize)? else {
                            return None;
                        };
                        let old = *slot;
                        let new = if increment { old + 1.0 } else { old - 1.0 };
                        *slot = new;
                        Some(Value::Number(if prefix { new } else { old }))
                    });
                    match fast {
                        Some(value) => self.vm_stack.push(value),
                        None => {
                            self.frames[top].pc = pc;
                            self.update_local_slow(top, depth, index, name, increment, prefix)?;
                        }
                    }
                }
                Op::This => {
                    if matches!(self.frames[top].this, Value::Uninitialized) {
                        self.frames[top].pc = pc;
                        return Err(self
                            .reference_error("must call super constructor before accessing this"));
                    }
                    let this = self.frames[top].this.clone();
                    self.vm_stack.push(this);
                }
                Op::Not => {
                    let value = self.pop();
                    let result = !value.truthy();
                    value.fast_drop();
                    self.vm_stack.push(Value::Bool(result));
                }
                Op::Binary(operator) => {
                    let len = self.vm_stack.len();
                    if len >= 2 {
                        if let (Value::Number(left), Value::Number(right)) =
                            (&self.vm_stack[len - 2], &self.vm_stack[len - 1])
                            && let Some(value) =
                                super::operations::number_binary(operator, *left, *right)
                        {
                            // safety: both operands are numbers with no drop glue
                            unsafe { self.vm_stack.set_len(len - 2) };
                            self.vm_stack.push(value);
                            continue;
                        }
                        if matches!(
                            operator,
                            super::ast::BinaryOperator::StrictEqual
                                | super::ast::BinaryOperator::StrictNotEqual
                        ) {
                            let equal =
                                values_equal(&self.vm_stack[len - 2], &self.vm_stack[len - 1]);
                            let right = self.vm_stack.pop().unwrap_or(Value::Undefined);
                            let left = self.vm_stack.pop().unwrap_or(Value::Undefined);
                            right.fast_drop();
                            left.fast_drop();
                            let result = equal
                                == matches!(operator, super::ast::BinaryOperator::StrictEqual);
                            self.vm_stack.push(Value::Bool(result));
                            continue;
                        }
                    }
                    self.frames[top].pc = pc;
                    let right = self.pop();
                    let left = self.pop();
                    let value = self.binary(operator, left, right)?;
                    self.vm_stack.push(value);
                }
                Op::Jump(target) => {
                    let target = target as usize;
                    if target <= op_pc {
                        self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                        if self.steps >= safepoint_at {
                            safepoint_at = self.hit_safepoint(sampling, target)?;
                        }
                    }
                    pc = target;
                    block_start = pc;
                }
                Op::JumpIfFalse(target) | Op::JumpIfTrue(target) => {
                    let value = self.pop();
                    let test = match &value {
                        Value::Bool(test) => *test,
                        other => other.truthy(),
                    };
                    value.fast_drop();
                    if test == matches!(op, Op::JumpIfTrue(_)) {
                        let target = target as usize;
                        if target <= op_pc {
                            self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                            if self.steps >= safepoint_at {
                                safepoint_at = self.hit_safepoint(sampling, target)?;
                            }
                        }
                        pc = target;
                        block_start = pc;
                    }
                }
                Op::JumpIfFalseKeep(target) | Op::JumpIfTrueKeep(target) => {
                    let test = match self.vm_stack.last() {
                        Some(Value::Bool(test)) => *test,
                        Some(other) => other.truthy(),
                        None => false,
                    };
                    if test == matches!(op, Op::JumpIfTrueKeep(_)) {
                        let target = target as usize;
                        if target <= op_pc {
                            self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                            if self.steps >= safepoint_at {
                                safepoint_at = self.hit_safepoint(sampling, target)?;
                            }
                        }
                        pc = target;
                        block_start = pc;
                    }
                }
                Op::JumpIfNotNullishKeep(target) => {
                    if !matches!(
                        self.vm_stack.last(),
                        Some(Value::Undefined | Value::Null) | None
                    ) {
                        pc = target as usize;
                        block_start = pc;
                    }
                }
                Op::JumpIfNotUndefined(target) => {
                    if !matches!(self.vm_stack.last(), Some(Value::Undefined) | None) {
                        pc = target as usize;
                        block_start = pc;
                    }
                }
                Op::NewObject => {
                    let object = self.new_plain_object();
                    self.vm_stack.push(Value::Object(object));
                }
                Op::MakeObject(template) => {
                    let (keys, cell) = unsafe { &(&*code).object_shapes[template as usize] };
                    let shape = cell
                        .get_or_init(|| {
                            let names: Vec<&str> = keys.iter().map(|name| &**name).collect();
                            super::object::Shape::from_keys(&names)
                        })
                        .clone();
                    let count = shape.len();
                    let start = self.vm_stack.len() - count;
                    let slots: Vec<Slot> = self.vm_stack.drain(start..).map(Slot::Data).collect();
                    let object = new_object(JsObject::from_shape(
                        shape,
                        slots,
                        Some(self.intrinsics.object_prototype.clone()),
                    ));
                    self.vm_stack.push(Value::Object(object));
                }
                Op::MakeArray(count) => {
                    let count = count as usize;
                    let values = if count == 0 {
                        Vec::new()
                    } else {
                        let start = self.vm_stack.len() - count;
                        self.vm_stack.split_off(start)
                    };
                    self.vm_stack.push(Self::new_array(values));
                }
                Op::GetProp { name, ic } => {
                    use super::object::IcGetHit;
                    let cache = unsafe { &(&*code).ics[ic as usize] };
                    let hit = match self.vm_stack.last() {
                        Some(Value::Object(handle)) => handle
                            .try_borrow()
                            .ok()
                            .and_then(|object| object.ic_get(cache)),
                        Some(Value::Element(handle)) => handle
                            .object
                            .try_borrow()
                            .ok()
                            .and_then(|object| object.ic_get(cache)),
                        Some(Value::Array(array)) => {
                            if cache.is_array_length() {
                                array
                                    .try_borrow()
                                    .ok()
                                    .map(|a| IcGetHit::Value(Value::Number(a.len() as f64)))
                            } else if array.try_borrow().ok().is_some_and(|a| a.extra.is_none()) {
                                JsObject::ic_array_get(&self.intrinsics.array_prototype, cache)
                                    .map(IcGetHit::Value)
                            } else {
                                None
                            }
                        }
                        Some(Value::String(text)) => {
                            if cache.is_string_length() {
                                Some(IcGetHit::Value(Value::Number(
                                    super::strings::utf16_length(text) as f64,
                                )))
                            } else {
                                JsObject::ic_string_get(&self.intrinsics.string_prototype, cache)
                                    .map(IcGetHit::Value)
                            }
                        }
                        _ => None,
                    };
                    match hit {
                        Some(IcGetHit::Value(value)) => {
                            if let Some(slot) = self.vm_stack.last_mut() {
                                let old = std::mem::replace(slot, value);
                                old.fast_drop();
                            }
                        }
                        Some(IcGetHit::Missing) => {
                            if let Some(slot) = self.vm_stack.last_mut() {
                                let old = std::mem::replace(slot, Value::Undefined);
                                old.fast_drop();
                            }
                        }
                        Some(IcGetHit::Getter(getter)) => {
                            // push getter before safepoint so GC traces it on vm_stack
                            self.vm_stack.push(getter);
                            self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                            if self.steps >= safepoint_at {
                                safepoint_at = self.hit_safepoint(sampling, op_pc)?;
                            }
                            self.frames[top].pc = pc;
                            if !self.call_plain(0, true) {
                                let callee = self.pop();
                                let this = self.pop();
                                self.call_in_loop(callee, this, Vec::new())?;
                            }
                            top = self.frames.len() - 1;
                            code = Rc::as_ptr(&self.frames[top].code);
                            pc = self.frames[top].pc;
                            reg_base = self.frames[top].reg_base;
                            block_start = pc;
                        }
                        None => {
                            self.frames[top].pc = pc;
                            if self.get_prop_slow(name, ic)? {
                                top = self.frames.len() - 1;
                                code = Rc::as_ptr(&self.frames[top].code);
                                pc = self.frames[top].pc;
                                reg_base = self.frames[top].reg_base;
                                block_start = pc;
                            }
                        }
                    }
                }
                Op::SetProp { name, ic } => {
                    let len = self.vm_stack.len();
                    let cache = unsafe { &(&*code).ics[ic as usize] };
                    let hit = len >= 2
                        && match &self.vm_stack[len - 2] {
                            Value::Object(handle) => {
                                let value = self.vm_stack[len - 1].clone();
                                handle
                                    .try_borrow_mut()
                                    .is_ok_and(|mut target| target.ic_set(cache, value).is_ok())
                            }
                            _ => false,
                        };
                    if hit {
                        let value = self.vm_stack.pop().unwrap_or(Value::Undefined);
                        let old = std::mem::replace(&mut self.vm_stack[len - 2], value);
                        old.fast_drop();
                    } else {
                        self.frames[top].pc = pc;
                        self.set_prop_slow(name, ic)?;
                    }
                }
                Op::Call(count) | Op::CallMethod(count) => {
                    self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                    if self.steps >= safepoint_at {
                        safepoint_at = self.hit_safepoint(sampling, op_pc)?;
                    }
                    self.frames[top].pc = pc;
                    let method = matches!(op, Op::CallMethod(_));
                    if !self.call_plain(count as usize, method) {
                        let arguments = self.pop_arguments(count as usize);
                        let callee = self.pop();
                        let this = if method { self.pop() } else { Value::Undefined };
                        self.call_in_loop(callee, this, arguments)?;
                    }
                    top = self.frames.len() - 1;
                    code = Rc::as_ptr(&self.frames[top].code);
                    pc = self.frames[top].pc;
                    reg_base = self.frames[top].reg_base;
                    block_start = pc;
                }
                Op::Return => {
                    let value = self.pop();
                    if self.frames[top].extra.is_none() && !self.frames[top].construct() {
                        let mut finished = self.frames.pop().expect("a frame is running");
                        truncate_stack(&mut self.vm_stack, finished.return_base);
                        if (finished.flags & (FLAG_OWNS_SCOPE | FLAG_SCOPE_ESCAPED))
                            == FLAG_OWNS_SCOPE
                            && let Some(scope) = finished.scope.take()
                        {
                            super::heap::free_scope(scope);
                        }
                        if self.frames.len() == frame_floor {
                            return Ok(RunResult::Return(value));
                        }
                        self.vm_stack.push(value);
                    } else {
                        self.frames[top].pc = pc;
                        if let Some(result) = self.do_return(frame_floor, value)? {
                            return Ok(result);
                        }
                    }
                    top = self.frames.len() - 1;
                    code = Rc::as_ptr(&self.frames[top].code);
                    pc = self.frames[top].pc;
                    reg_base = self.frames[top].reg_base;
                    block_start = pc;
                }
                other => {
                    self.frames[top].pc = pc;
                    if let Some(result) = self.step_op(other, frame_floor)? {
                        return Ok(result);
                    }
                    top = self.frames.len() - 1;
                    code = Rc::as_ptr(&self.frames[top].code);
                    pc = self.frames[top].pc;
                    reg_base = self.frames[top].reg_base;
                    block_start = pc;
                }
            }
        }
    }

    fn update_reg_slow(
        &mut self,
        top: usize,
        reg: u16,
        name: u32,
        increment: bool,
        prefix: bool,
    ) -> Result<(), ScriptError> {
        let slot_index = self.frames[top].reg_base + reg as usize;
        let current = self.vm_stack[slot_index].clone();
        if matches!(current, Value::Uninitialized) {
            let name = self.frames[top].code.names[name as usize].clone();
            return Err(self.reference_error(format!("cannot access {name} before initialization")));
        }
        let delta = if increment { 1 } else { -1 };
        let (old, new) = match current {
            Value::Number(number) => (
                Value::Number(number),
                Value::Number(number + f64::from(delta)),
            ),
            other => match self.coerce_numeric(other)? {
                Value::BigInt(value) => {
                    let stepped = value.add(&super::bigint::BigInt::from_i64(i64::from(delta)));
                    (Value::BigInt(value), Value::BigInt(Rc::new(stepped)))
                }
                numeric => {
                    let number = self.coerce_number(numeric)?;
                    (
                        Value::Number(number),
                        Value::Number(number + f64::from(delta)),
                    )
                }
            },
        };
        let replaced = std::mem::replace(&mut self.vm_stack[slot_index], new.clone());
        replaced.fast_drop();
        self.vm_stack.push(if prefix { new } else { old });
        Ok(())
    }

    fn update_local_slow(
        &mut self,
        top: usize,
        depth: u16,
        index: u16,
        name: u32,
        increment: bool,
        prefix: bool,
    ) -> Result<(), ScriptError> {
        let current = self.read_local(top, depth, index, name)?;
        let delta = if increment { 1 } else { -1 };
        let (old, new) = match current {
            Value::Number(number) => (
                Value::Number(number),
                Value::Number(number + f64::from(delta)),
            ),
            other => match self.coerce_numeric(other)? {
                Value::BigInt(value) => {
                    let stepped = value.add(&super::bigint::BigInt::from_i64(i64::from(delta)));
                    (Value::BigInt(value), Value::BigInt(Rc::new(stepped)))
                }
                numeric => {
                    let number = self.coerce_number(numeric)?;
                    (
                        Value::Number(number),
                        Value::Number(number + f64::from(delta)),
                    )
                }
            },
        };
        self.write_local(top, depth, index, name, new.clone())?;
        self.vm_stack.push(if prefix { new } else { old });
        Ok(())
    }

    fn get_prop_slow(&mut self, name: u32, ic: u32) -> Result<bool, ScriptError> {
        let object = self.pop();
        let top = self.frames.len() - 1;
        let code = self.frames[top].code.clone();
        let cache = &code.ics[ic as usize];
        let key = code.names[name as usize].clone();
        let outcome = self.get_property_tail(&object, &key)?;
        match &object {
            Value::Object(handle) => {
                if let Ok(object) = handle.try_borrow() {
                    object.record_get_ic(&key, cache);
                }
            }
            Value::Element(handle) => {
                if let Ok(object) = handle.object.try_borrow() {
                    object.record_get_ic(&key, cache);
                }
            }
            Value::Array(array) => {
                if &*key == "length" {
                    cache.record_array_length();
                } else if array.borrow().extra.is_none() && key.parse::<usize>().is_err() {
                    JsObject::record_array_method_ic(&self.intrinsics.array_prototype, &key, cache);
                }
            }
            Value::String(_) => {
                if &*key == "length" {
                    cache.record_string_length();
                } else if super::object::array_index(&key).is_none() {
                    JsObject::record_string_method_ic(
                        &self.intrinsics.string_prototype,
                        &key,
                        cache,
                    );
                }
            }
            _ => {}
        }
        match outcome {
            super::operations::GetOutcome::Value(value) => {
                self.vm_stack.push(value);
                Ok(false)
            }
            super::operations::GetOutcome::Call {
                callee,
                this,
                arguments,
            } => {
                self.call_in_loop(callee, this, arguments)?;
                Ok(true)
            }
        }
    }

    fn set_prop_slow(&mut self, name: u32, ic: u32) -> Result<(), ScriptError> {
        let value = self.pop();
        let object = self.pop();
        let top = self.frames.len() - 1;
        let code = self.frames[top].code.clone();
        let cache = &code.ics[ic as usize];
        let key = code.names[name as usize].clone();
        let strict = code.strict;
        let from_shape = match &object {
            Value::Object(handle) => handle.try_borrow().ok().and_then(|o| o.shape_handle()),
            _ => None,
        };
        self.set_property(&object, &key, value.clone(), strict)?;
        if let Value::Object(handle) = &object
            && let Ok(target) = handle.try_borrow()
        {
            target.record_set_ic(&key, from_shape, cache);
        }
        self.vm_stack.push(value);
        Ok(())
    }

    // executes one opcode on the top frame; Some ends this run with a result
    fn step_op(&mut self, op: Op, frame_floor: usize) -> Result<Option<RunResult>, ScriptError> {
        let top = self.frames.len() - 1;
        match op {
            Op::Nop => {}
            Op::Undefined => self.vm_stack.push(Value::Undefined),
            Op::Null => self.vm_stack.push(Value::Null),
            Op::True => self.vm_stack.push(Value::Bool(true)),
            Op::False => self.vm_stack.push(Value::Bool(false)),
            Op::Number(value) => self.vm_stack.push(Value::Number(value)),
            Op::BigInt(index) => {
                let value =
                    super::bigint::BigInt::parse(&self.frames[top].code.names[index as usize])
                        .expect("the lexer produced decimal digits");
                self.vm_stack.push(Value::BigInt(Rc::new(value)));
            }
            // the literal's text is shared, not copied, each time it is read
            Op::String(index) => {
                let text = self.frames[top].code.names[index as usize].clone();
                self.vm_stack.push(Value::String(text));
            }
            Op::Regex { pattern, flags } => {
                let code = self.frames[top].code.clone();
                let regex =
                    self.new_regexp(&code.names[pattern as usize], &code.names[flags as usize])?;
                self.vm_stack.push(regex);
            }
            Op::TemplateObject(index) => {
                let code = self.frames[top].code.clone();
                let value = self.template_object(&code, index as usize);
                self.vm_stack.push(value);
            }
            Op::Pop => {
                if let Some(value) = self.vm_stack.pop() {
                    value.fast_drop();
                }
            }
            Op::Dup => {
                let value = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                self.vm_stack.push(value);
            }
            Op::Dup2 => {
                let length = self.vm_stack.len();
                let a = self.vm_stack[length - 2].clone();
                let b = self.vm_stack[length - 1].clone();
                self.vm_stack.push(a);
                self.vm_stack.push(b);
            }
            Op::Dup3 => {
                let length = self.vm_stack.len();
                let a = self.vm_stack[length - 3].clone();
                let b = self.vm_stack[length - 2].clone();
                let c = self.vm_stack[length - 1].clone();
                self.vm_stack.push(a);
                self.vm_stack.push(b);
                self.vm_stack.push(c);
            }
            Op::RotateUp4 => {
                let length = self.vm_stack.len();
                let value = self.vm_stack.remove(length - 4);
                self.vm_stack.push(value);
            }
            Op::RotateDown5 => {
                let value = self.pop();
                let length = self.vm_stack.len();
                self.vm_stack.insert(length - 4, value);
            }
            Op::Swap => {
                let length = self.vm_stack.len();
                self.vm_stack.swap(length - 1, length - 2);
            }
            Op::RotateDown3 => {
                let value = self.pop();
                let length = self.vm_stack.len();
                self.vm_stack.insert(length - 2, value);
            }
            Op::RotateDown4 => {
                let value = self.pop();
                let length = self.vm_stack.len();
                self.vm_stack.insert(length - 3, value);
            }
            Op::RotateUp3 => {
                let length = self.vm_stack.len();
                let value = self.vm_stack.remove(length - 3);
                self.vm_stack.push(value);
            }
            Op::RequireCoercible => {
                if matches!(
                    self.vm_stack.last(),
                    Some(Value::Undefined | Value::Null) | None
                ) {
                    return Err(self.type_error("cannot destructure null or undefined"));
                }
            }
            Op::CopyRest => {
                let keys = self.pop();
                let source = self.pop();
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
                self.vm_stack.push(Value::Object(copy));
            }
            Op::GetReg { reg, name } => {
                let slot_index = self.frames[top].reg_base + reg as usize;
                let value = self.vm_stack[slot_index].clone();
                if matches!(value, Value::Uninitialized) {
                    let name = self.frames[top].code.names[name as usize].clone();
                    return Err(
                        self.reference_error(format!("cannot access {name} before initialization"))
                    );
                }
                self.vm_stack.push(value);
            }
            Op::SetReg { reg, name } => {
                let slot_index = self.frames[top].reg_base + reg as usize;
                if matches!(self.vm_stack[slot_index], Value::Uninitialized) {
                    let name = self.frames[top].code.names[name as usize].clone();
                    return Err(
                        self.reference_error(format!("cannot access {name} before initialization"))
                    );
                }
                let value = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                let old = std::mem::replace(&mut self.vm_stack[slot_index], value);
                old.fast_drop();
            }
            Op::InitReg(reg) => {
                let value = self.pop();
                let slot_index = self.frames[top].reg_base + reg as usize;
                let old = std::mem::replace(&mut self.vm_stack[slot_index], value);
                old.fast_drop();
            }
            Op::UpdateReg {
                reg,
                name,
                increment,
                prefix,
            } => {
                self.update_reg_slow(top, reg, name, increment, prefix)?;
            }
            Op::UninitRegs { index, count } => {
                let start = self.frames[top].reg_base + index as usize;
                let end = start + count as usize;
                for slot in &mut self.vm_stack[start..end] {
                    let old = std::mem::replace(slot, Value::Uninitialized);
                    old.fast_drop();
                }
            }
            Op::GetLocal { depth, index, name } => {
                let value = self.read_local(top, depth, index, name)?;
                self.vm_stack.push(value);
            }
            Op::SetLocal { depth, index, name } => {
                let value = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                self.write_local(top, depth, index, name, value)?;
            }
            Op::UpdateLocal {
                depth,
                index,
                name,
                increment,
                prefix,
            } => {
                self.update_local_slow(top, depth, index, name, increment, prefix)?;
            }
            Op::InitLocal { depth, index } => {
                let value = self.pop();
                let scope_ref = Self::scope_at(&self.frames[top], depth);
                let mut scope = scope_ref.borrow_mut();
                match &scope.slots[index as usize] {
                    Value::Binding(cell) | Value::ConstBinding(cell) => *cell.borrow_mut() = value,
                    _ => scope.slots[index as usize] = value,
                }
            }
            Op::GetGlobal(name) => {
                let name = self.frames[top].code.names[name as usize].clone();
                let value = self.get_global(&name)?;
                self.vm_stack.push(value);
            }
            Op::GetName { name, depth, index } => {
                let (_, value) = self.read_name(top, name, depth, index)?;
                if matches!(value, Value::Uninitialized) {
                    let name = self.frames[top].code.names[name as usize].clone();
                    return Err(
                        self.reference_error(format!("cannot access {name} before initialization"))
                    );
                }
                self.vm_stack.push(value);
            }
            Op::CalleeName { name, depth, index } => {
                let (this, callee) = self.read_name(top, name, depth, index)?;
                self.vm_stack.push(this);
                self.vm_stack.push(callee);
            }
            Op::TypeofName { name, depth, index } => {
                let key = self.frames[top].code.names[name as usize].clone();
                let scope = self.frames[top].scope.clone();
                let value = match self.shadowing(scope, &key, depth)? {
                    Some(Shadow::With(object)) => self.get_property(&object, &key)?,
                    Some(Shadow::Grown(scope, index)) => scope.borrow().slots[index].clone(),
                    None if depth == GLOBAL => match self.get_global_accessor(&key) {
                        Some(value) => value?,
                        None => self.lookup_global(&key).unwrap_or(Value::Undefined),
                    },
                    None => self.read_local(top, depth, index, name)?,
                };
                let kind = self.type_of(&value);
                self.vm_stack.push(Value::String(type_name(kind)));
            }
            Op::SetName {
                name,
                depth,
                index,
                kind,
            } => {
                let key = self.frames[top].code.names[name as usize].clone();
                let value = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                let strict = self.frames[top].code.strict;
                let scope = self.frames[top].scope.clone();
                match self.shadowing(scope, &key, depth)? {
                    Some(Shadow::With(object)) => {
                        self.set_property(&object, &key, value, strict)?
                    }
                    Some(Shadow::Grown(scope, index)) => scope.borrow_mut().slots[index] = value,
                    None if depth == GLOBAL => self.set_global(&key, value, strict)?,
                    None => match kind {
                        BindingKind::Const | BindingKind::Import => {
                            return Err(
                                self.type_error(format!("Assignment to constant variable '{key}'"))
                            );
                        }
                        BindingKind::SelfName if strict => {
                            return Err(
                                self.type_error(format!("Assignment to constant variable '{key}'"))
                            );
                        }
                        // a sloppy write to a function expression's own name is dropped
                        BindingKind::SelfName => {}
                        _ => self.write_local(top, depth, index, name, value)?,
                    },
                }
            }
            Op::DeleteName { name, depth } => {
                let key = self.frames[top].code.names[name as usize].clone();
                let strict = self.frames[top].code.strict;
                let scope = self.frames[top].scope.clone();
                let deleted = match self.shadowing(scope, &key, depth)? {
                    Some(Shadow::With(object)) => self.delete_property(&object, &key, strict)?,
                    // an eval's binding is deletable, as the eval declaration steps made it
                    Some(Shadow::Grown(scope, index)) => {
                        let mut scope = scope.borrow_mut();
                        let position = index - scope.layout.names.len();
                        scope.extra[position] = "".into();
                        scope.slots[index] = Value::Undefined;
                        true
                    }
                    None if depth == GLOBAL => self.delete_property(&Value::Window, &key, false)?,
                    None => false,
                };
                self.vm_stack.push(Value::Bool(deleted));
            }
            Op::DeclareLexical(name) => {
                let name = self.frames[top].code.names[name as usize].to_string();
                self.global.borrow_mut().lexicals.insert(name);
            }
            Op::TypeofGlobal(name) => {
                let name = self.frames[top].code.names[name as usize].clone();
                let value = match self.get_global_accessor(&name) {
                    Some(value) => value?,
                    None => self.lookup_global(&name).unwrap_or(Value::Undefined),
                };
                let kind = self.type_of(&value);
                self.vm_stack.push(Value::String(type_name(kind)));
            }
            Op::SetGlobal(name) | Op::InitGlobal(name) => {
                let name = self.frames[top].code.names[name as usize].clone();
                let value = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                let strict = self.frames[top].code.strict;
                let initialize = matches!(op, Op::InitGlobal(_));
                // a function declaration replaces a configurable property outright, as
                // CreateGlobalFunctionBinding defines it
                if initialize {
                    let mut global = self.global.borrow_mut();
                    if global
                        .accessors
                        .get(&*name)
                        .is_some_and(|property| property.configurable)
                    {
                        global.accessors.remove(&*name);
                    }
                }
                self.set_global(&name, value, strict && !initialize)?;
            }
            Op::InitGlobalConst(name) => {
                let name = self.frames[top].code.names[name as usize].to_string();
                let value = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                self.global
                    .borrow_mut()
                    .values
                    .insert(name, Value::ConstBinding(Rc::new(RefCell::new(value))));
            }
            Op::DeclareGlobal(name) => {
                let name = self.frames[top].code.names[name as usize].clone();
                if self.lookup_global(&name).is_none()
                    && !self.global.borrow().accessors.contains_key(&*name)
                {
                    self.global
                        .borrow_mut()
                        .values
                        .insert(name.to_string(), Value::Undefined);
                }
            }
            Op::DeleteGlobal(name) => {
                let name = self.frames[top].code.names[name as usize].clone();
                let deleted = self.delete_property(&Value::Window, &name, false)?;
                self.vm_stack.push(Value::Bool(deleted));
            }
            Op::PushScope(block) => {
                let layout = self.frames[top].code.blocks[block as usize].clone();
                let parent = self.frames[top].scope.clone();
                let scope = self.new_scope(layout, parent);
                // lexical bindings in a block start in their temporal dead zone
                for slot in scope.borrow_mut().slots.iter_mut() {
                    *slot = Value::Uninitialized;
                }
                let frame = &mut self.frames[top];
                frame.flags |= FLAG_SCOPE_ESCAPED;
                frame.scope = Some(scope);
                frame.scopes += 1;
            }
            Op::PushVarScope(block) => {
                let layout = self.frames[top].code.blocks[block as usize].clone();
                let parent = self.frames[top].scope.clone();
                // a function's variable environment, whose bindings start undefined
                let scope = self.new_scope(layout, parent);
                let frame = &mut self.frames[top];
                frame.flags |= FLAG_SCOPE_ESCAPED;
                frame.scope = Some(scope);
                frame.scopes += 1;
            }
            Op::Uninitialize { index, count } => {
                if let Some(scope) = &self.frames[top].scope {
                    let mut scope = scope.borrow_mut();
                    let range =
                        index as usize..(index as usize + count as usize).min(scope.slots.len());
                    scope.slots[range].fill(Value::Uninitialized);
                }
            }
            Op::PopScope => {
                let target = self.frames[top].scopes.saturating_sub(1);
                Self::restore_scopes(&mut self.frames[top], target);
            }
            Op::Debugger => {}
            Op::CloneScope => {
                self.frames[top].flags |= FLAG_SCOPE_ESCAPED;
                let scope = self.frames[top].scope.clone().expect("a scope to copy");
                let scope = scope.borrow();
                let copy = Scope::new(scope.layout.clone(), scope.parent.clone());
                copy.borrow_mut().slots = scope.slots.clone();
                copy.borrow_mut().extra = scope.extra.clone();
                drop(scope);
                self.frames[top].scope = Some(copy);
            }
            Op::This => {
                if matches!(self.frames[top].this, Value::Uninitialized) {
                    return Err(
                        self.reference_error("must call super constructor before accessing this")
                    );
                }
                let this = self.frames[top].this.clone();
                self.vm_stack.push(this);
            }
            Op::GlobalThis => self.vm_stack.push(Value::Window),
            Op::NewTarget => {
                let target = self.frames[top].new_target_value();
                self.vm_stack.push(target);
            }
            Op::Home => {
                let home = self.frames[top]
                    .closure
                    .as_ref()
                    .and_then(|closure| closure.home())
                    .map(Value::Object)
                    .unwrap_or(Value::Undefined);
                self.vm_stack.push(home);
            }
            Op::ImportMeta => {
                let url = self.frames[top].code.url.get().cloned();
                let meta = match url {
                    Some(url) => {
                        let key = url.to_string();
                        match self.import_metas.get(&key) {
                            Some(meta) => meta.clone(),
                            None => {
                                let meta = self.new_plain_object();
                                meta.borrow_mut()
                                    .insert("url".to_string(), Value::String(key.as_str().into()));
                                self.import_metas.insert(key, meta.clone());
                                meta
                            }
                        }
                    }
                    None => self.new_plain_object(),
                };
                self.vm_stack.push(Value::Object(meta));
            }
            Op::InitThisSlot(slot) => {
                let value = self.frames[top].this.clone();
                Self::scope_at(&self.frames[top], 0).borrow_mut().slots[slot as usize] = value;
            }
            Op::InitNewTargetSlot(slot) => {
                let value = self.frames[top].new_target_value();
                Self::scope_at(&self.frames[top], 0).borrow_mut().slots[slot as usize] = value;
            }
            Op::InitHomeSlot(slot) => {
                let home = self.frames[top]
                    .closure
                    .as_ref()
                    .and_then(|closure| closure.home())
                    .map(Value::Object)
                    .unwrap_or(Value::Undefined);
                Self::scope_at(&self.frames[top], 0).borrow_mut().slots[slot as usize] = home;
            }
            Op::InitArgumentsSlot(slot) => {
                let arguments = self.frames[top]
                    .extra
                    .as_ref()
                    .map(|extra| extra.arguments.clone())
                    .unwrap_or_default();
                let object = self.arguments_object(arguments);
                Self::scope_at(&self.frames[top], 0).borrow_mut().slots[slot as usize] = object;
            }
            Op::NewObject => {
                let object = self.new_plain_object();
                self.vm_stack.push(Value::Object(object));
            }
            Op::MakeObject(template) => {
                let shape = {
                    let (keys, cell) = &self.frames[top].code.object_shapes[template as usize];
                    cell.get_or_init(|| {
                        let names: Vec<&str> = keys.iter().map(|name| &**name).collect();
                        super::object::Shape::from_keys(&names)
                    })
                    .clone()
                };
                let count = shape.len();
                let start = self.vm_stack.len() - count;
                let slots: Vec<Slot> = self.vm_stack.drain(start..).map(Slot::Data).collect();
                let object = new_object(JsObject::from_shape(
                    shape,
                    slots,
                    Some(self.intrinsics.object_prototype.clone()),
                ));
                self.vm_stack.push(Value::Object(object));
            }
            Op::NewArray => self.vm_stack.push(Self::new_array(Vec::new())),
            Op::MakeArray(count) => {
                let count = count as usize;
                let values = if count == 0 {
                    Vec::new()
                } else {
                    let start = self.vm_stack.len() - count;
                    self.vm_stack.split_off(start)
                };
                self.vm_stack.push(Self::new_array(values));
            }
            Op::ArrayPush => {
                let value = self.pop();
                if let Some(Value::Array(array)) = self.vm_stack.last() {
                    array.borrow_mut().push(value);
                }
            }
            Op::ArrayHole => {
                if let Some(Value::Array(array)) = self.vm_stack.last() {
                    array.borrow_mut().push(Value::Uninitialized);
                }
            }
            Op::ArraySpread(_) | Op::ArgumentSpread(_) => {
                let value = self.pop();
                let code = self.frames[top].code.clone();
                let name = |index: u32| code.names[index as usize].clone();
                self.iterable_callsite = match op {
                    Op::ArraySpread(message) => message.map(name).map(Callsite::Source),
                    Op::ArgumentSpread(site) => Some(Callsite::Argument(name(site))),
                    _ => None,
                };
                let items = self.iterate_to_vec(value);
                self.iterable_callsite = None;
                let items = items?;
                if let Some(Value::Array(array)) = self.vm_stack.last().cloned() {
                    let mut array = array.borrow_mut();
                    if array.len() + items.len() > MAX_RUNTIME_ARRAY_LENGTH {
                        drop(array);
                        return Err(self.range_error("array exceeds the length limit"));
                    }
                    array.extend(items);
                }
            }
            Op::DefineField(name) => {
                let value = self.pop();
                if let Some(Value::Object(object)) = self.vm_stack.last() {
                    object.borrow_mut().define_key(
                        &self.frames[top].code.names[name as usize],
                        Property::data(value),
                    );
                }
            }
            Op::DefineComputed => {
                let value = self.pop();
                let key = self.pop().property_key();
                if let Some(Value::Object(object)) = self.vm_stack.last() {
                    object.borrow_mut().define(key, Property::data(value));
                }
            }
            Op::DefineAccessor {
                getter,
                computed,
                name,
            } => {
                let function = self.pop();
                let key = if computed {
                    self.pop().property_key()
                } else {
                    self.frames[top].code.names[name as usize].to_string()
                };
                let Some(Value::Object(object)) = self.vm_stack.last().cloned() else {
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
                let source = self.pop();
                let entries = self.own_enumerable_entries(&source)?;
                if let Some(Value::Object(object)) = self.vm_stack.last() {
                    let mut object = object.borrow_mut();
                    for (key, value) in entries {
                        object.define(key, Property::data(value));
                    }
                }
            }
            Op::SetPrototype => {
                let prototype = self.pop();
                if let Some(Value::Object(object)) = self.vm_stack.last() {
                    match prototype {
                        Value::Object(prototype) => {
                            object.borrow_mut().set_prototype(Some(prototype))
                        }
                        Value::Null => object.borrow_mut().set_prototype(None),
                        _ => {}
                    }
                }
            }
            Op::GetProp { name, ic } => {
                self.get_prop_slow(name, ic)?;
            }
            Op::GetIndex => {
                let key = self.pop();
                let object = self.pop();
                match self.get_index_tail(&object, key)? {
                    super::operations::GetOutcome::Value(value) => self.vm_stack.push(value),
                    super::operations::GetOutcome::Call {
                        callee,
                        this,
                        arguments,
                    } => {
                        self.call_in_loop(callee, this, arguments)?;
                    }
                }
            }
            Op::SetProp { name, ic } => {
                self.set_prop_slow(name, ic)?;
            }
            Op::SetIndex => {
                let value = self.pop();
                let key = self.pop();
                let object = self.pop();
                let strict = self.frames[top].code.strict;
                self.set_index(&object, key, value.clone(), strict)?;
                self.vm_stack.push(value);
            }
            Op::WithHas(name) => {
                let object = self.pop();
                let key = self.frames[top].code.names[name as usize].clone();
                let has = self.with_has(&object, &key)?;
                self.vm_stack.push(object);
                self.vm_stack.push(Value::Bool(has));
            }
            Op::DeleteProp(name) => {
                let object = self.pop();
                let key = self.frames[top].code.names[name as usize].clone();
                let strict = self.frames[top].code.strict;
                let deleted = self.delete_property(&object, &key, strict)?;
                self.vm_stack.push(Value::Bool(deleted));
            }
            Op::DeleteIndex => {
                let key = self.pop();
                let object = self.pop();
                let strict = self.frames[top].code.strict;
                let key = self.coerce_key(key)?;
                let deleted = self.delete_property(&object, &key, strict)?;
                self.vm_stack.push(Value::Bool(deleted));
            }
            Op::GetSuper(name) => {
                let home = self.pop();
                let this = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                let key = self.frames[top].code.names[name as usize].clone();
                let value = self.get_super(&home, &this, &key)?;
                self.vm_stack.push(value);
            }
            Op::GetSuperIndex => {
                let key = self.pop();
                let home = self.pop();
                let this = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                let key = self.coerce_key(key)?;
                let value = self.get_super(&home, &this, &key)?;
                self.vm_stack.push(value);
            }
            Op::SetSuper(name) => {
                let value = self.pop();
                let _home = self.pop();
                let this = self.pop();
                let key = self.frames[top].code.names[name as usize].clone();
                self.set_property(&this, &key, value.clone(), true)?;
                self.vm_stack.push(value);
            }
            Op::SetSuperIndex => {
                let value = self.pop();
                let key = self.pop();
                let _home = self.pop();
                let this = self.pop();
                self.set_index(&this, key, value.clone(), true)?;
                self.vm_stack.push(value);
            }
            Op::GetPrivate => {
                let key = self.pop().property_key();
                let object = self.pop();
                let value = self.get_private(&object, &key)?;
                self.vm_stack.push(value);
            }
            Op::SetPrivate => {
                let value = self.pop();
                let key = self.pop().property_key();
                let object = self.pop();
                self.set_private(&object, &key, value.clone())?;
                self.vm_stack.push(value);
            }
            Op::HasPrivate => {
                let object = self.pop();
                let key = self.pop().property_key();
                let Value::Object(object) = object else {
                    return Err(self.type_error("private brand checks need an object"));
                };
                let has = object.borrow().contains_key(&key);
                self.vm_stack.push(Value::Bool(has));
            }
            Op::NewPrivateName(name) => {
                self.private_names += 1;
                let key = format!(
                    "{}{}:{}",
                    PRIVATE_PREFIX, self.private_names, self.frames[top].code.names[name as usize]
                );
                self.vm_stack.push(Value::String(key.into()));
            }
            Op::Call(count) | Op::CallMethod(count) => {
                let method = matches!(op, Op::CallMethod(_));
                if !self.call_plain(count as usize, method) {
                    let arguments = self.pop_arguments(count as usize);
                    let callee = self.pop();
                    let this = if method { self.pop() } else { Value::Undefined };
                    self.call_in_loop(callee, this, arguments)?;
                }
            }
            Op::CallEval(_) | Op::CallEvalSpread => {
                let arguments = match op {
                    Op::CallEval(count) => self.pop_arguments(count as usize),
                    _ => match self.pop() {
                        Value::Array(array) => {
                            array.borrow().iter().cloned().map(Value::filled).collect()
                        }
                        _ => Vec::new(),
                    },
                };
                let callee = self.pop();
                let this = self.pop();
                let direct = matches!(
                    callee.resolve(),
                    Value::Object(ref object)
                        if matches!(object.borrow().class, ObjectClass::Native(ref native) if matches!(**native, Native::Eval))
                );
                if !direct {
                    return self.call_in_loop(callee, this, arguments);
                }
                return match arguments.into_iter().next() {
                    Some(Value::String(source)) => self.direct_eval(&source),
                    first => {
                        self.vm_stack.push(first.unwrap_or(Value::Undefined));
                        Ok(None)
                    }
                };
            }
            Op::CallSpread | Op::CallMethodSpread => {
                let list = self.pop();
                let callee = self.pop();
                let this = if matches!(op, Op::CallMethodSpread) {
                    self.pop()
                } else {
                    Value::Undefined
                };
                let arguments = match list {
                    Value::Array(array) => {
                        array.borrow().iter().cloned().map(Value::filled).collect()
                    }
                    _ => Vec::new(),
                };
                return self.call_in_loop(callee, this, arguments);
            }
            Op::New(count) => {
                let arguments = self.pop_arguments(count as usize);
                let callee = self.pop();
                return self.construct_in_loop(callee, arguments, None);
            }
            Op::NewSpread => {
                let list = self.pop();
                let callee = self.pop();
                let arguments = match list {
                    Value::Array(array) => {
                        array.borrow().iter().cloned().map(Value::filled).collect()
                    }
                    _ => Vec::new(),
                };
                return self.construct_in_loop(callee, arguments, None);
            }
            Op::SuperCall(count) => {
                let arguments = self.pop_arguments(count as usize);
                return self.super_call(frame_floor, arguments);
            }
            Op::SuperCallSpread => {
                let list = self.pop();
                let arguments = match list {
                    Value::Array(array) => {
                        array.borrow().iter().cloned().map(Value::filled).collect()
                    }
                    _ => Vec::new(),
                };
                return self.super_call(frame_floor, arguments);
            }
            Op::Closure(index) => {
                let code = self.frames[top].code.children[index as usize].clone();
                let scope = if code.captures_nothing {
                    None
                } else {
                    self.frames[top].flags |= FLAG_SCOPE_ESCAPED;
                    self.frames[top].scope.clone()
                };
                let home = self.frames[top]
                    .closure
                    .as_ref()
                    .and_then(|closure| closure.home());
                let function = self.new_closure(code.clone(), scope);
                // arrows and nested methods keep the home object of where they were made
                if code.kind == FunctionKind::Arrow
                    && let (Some(home), Some(closure)) = (home, Self::closure_of(&function))
                {
                    closure.set_home_object(home);
                }
                self.vm_stack.push(function);
            }
            Op::Class {
                template,
                has_super,
            } => {
                let template = self.frames[top].code.classes[template as usize].clone();
                let superclass = if has_super { Some(self.pop()) } else { None };
                self.frames[top].flags |= FLAG_SCOPE_ESCAPED;
                let scope = self.frames[top].scope.clone();
                let (constructor, prototype) = self.create_class(&template, superclass, scope)?;
                self.vm_stack.push(constructor);
                self.vm_stack.push(prototype);
            }
            Op::DefineMethod {
                is_static,
                kind,
                computed,
                name,
            } => {
                let function = self.pop();
                let key = if computed {
                    self.pop().property_key()
                } else {
                    self.frames[top].code.names[name as usize].to_string()
                };
                let length = self.vm_stack.len();
                let (Value::Object(constructor), Value::Object(prototype)) = (
                    self.vm_stack[length - 2].clone(),
                    self.vm_stack[length - 1].clone(),
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
                let function = self.pop();
                let key = if computed {
                    self.pop().property_key()
                } else {
                    self.frames[top].code.names[name as usize].to_string()
                };
                let Some(Value::Object(object)) = self.vm_stack.last().cloned() else {
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
                let initializer = self.pop();
                let key = if name == u32::MAX {
                    String::new()
                } else if computed {
                    self.pop().property_key()
                } else {
                    self.frames[top].code.names[name as usize].to_string()
                };
                let length = self.vm_stack.len();
                let (Value::Object(constructor), Value::Object(prototype)) = (
                    self.vm_stack[length - 2].clone(),
                    self.vm_stack[length - 1].clone(),
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
                closure.add_class_field(is_static, definition);
            }
            Op::RunStaticInit => {
                let constructor = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                if let Some(closure) = Self::closure_of(&constructor) {
                    let statics = closure.take_statics();
                    for field in statics {
                        self.install_field(&field, &constructor)?;
                    }
                }
            }
            Op::Binary(operator) => {
                let right = self.pop();
                let left = self.pop();
                let value = self.binary(operator, left, right)?;
                self.vm_stack.push(value);
            }
            Op::Negate => {
                let value = self.pop();
                let result = self.negate_numeric(value)?;
                self.vm_stack.push(result);
            }
            Op::Plus => {
                let value = self.pop();
                let number = self.coerce_number(value)?;
                self.vm_stack.push(Value::Number(number));
            }
            Op::ToNumeric => {
                let value = self.pop();
                let numeric = self.coerce_numeric(value)?;
                self.vm_stack.push(numeric);
            }
            Op::Not => {
                let value = self.pop();
                let result = !value.truthy();
                value.fast_drop();
                self.vm_stack.push(Value::Bool(result));
            }
            Op::BitNot => {
                let value = self.pop();
                let result = match self.coerce_numeric(value)? {
                    Value::BigInt(value) => Value::BigInt(Rc::new(value.not())),
                    other => Value::Number(f64::from(!super::operations::to_int32_number(
                        self.coerce_number(other)?,
                    ))),
                };
                self.vm_stack.push(result);
            }
            Op::TypeOf => {
                let value = self.pop();
                let kind = self.type_of(&value);
                value.fast_drop();
                self.vm_stack.push(Value::String(type_name(kind)));
            }
            Op::ToIndexKey if matches!(self.vm_stack.last(), Some(Value::Number(number)) if *number >= 0.0 && number.fract() == 0.0 && *number < 4_294_967_295.0) =>
                {}
            Op::ToPropertyKey | Op::ToIndexKey => {
                let value = self.pop();
                let key = self.coerce_key(value)?;
                self.vm_stack.push(Value::String(key.into()));
            }
            Op::Increment | Op::Decrement => {
                let value = self.pop();
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
                self.vm_stack.push(result);
            }
            Op::TemplateConcat(count) => {
                let parts = self.pop_arguments(count as usize);
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
                self.vm_stack.push(Value::String(text.into()));
            }
            Op::Jump(target) => self.frames[top].pc = target as usize,
            Op::JumpIfFalse(target) => {
                let value = self.pop();
                let test = match &value {
                    Value::Bool(test) => *test,
                    other => other.truthy(),
                };
                value.fast_drop();
                if !test {
                    self.frames[top].pc = target as usize;
                }
            }
            Op::JumpIfTrue(target) => {
                let value = self.pop();
                let test = match &value {
                    Value::Bool(test) => *test,
                    other => other.truthy(),
                };
                value.fast_drop();
                if test {
                    self.frames[top].pc = target as usize;
                }
            }
            Op::JumpIfFalseKeep(target) => {
                if !self.vm_stack.last().is_some_and(Value::truthy) {
                    self.frames[top].pc = target as usize;
                }
            }
            Op::JumpIfTrueKeep(target) => {
                if self.vm_stack.last().is_some_and(Value::truthy) {
                    self.frames[top].pc = target as usize;
                }
            }
            Op::JumpIfNotNullishKeep(target) => {
                if !matches!(
                    self.vm_stack.last(),
                    Some(Value::Undefined | Value::Null) | None
                ) {
                    self.frames[top].pc = target as usize;
                }
            }
            Op::JumpIfNotUndefined(target) => {
                if !matches!(self.vm_stack.last(), Some(Value::Undefined) | None) {
                    self.frames[top].pc = target as usize;
                }
            }
            Op::ChainStart => {
                let offset = self.vm_stack.len() - self.frames[top].stack_base;
                self.frames[top].extra_mut().chains.push(offset);
            }
            Op::JumpIfNullishChain(target) => {
                if matches!(
                    self.vm_stack.last(),
                    Some(Value::Undefined | Value::Null) | None
                ) {
                    let stack_base = self.frames[top].stack_base;
                    let mark = self.frames[top]
                        .extra
                        .as_mut()
                        .and_then(|extra| extra.chains.pop())
                        .map_or(self.vm_stack.len(), |offset| stack_base + offset);
                    truncate_stack(&mut self.vm_stack, mark);
                    self.vm_stack.push(Value::Undefined);
                    self.frames[top].pc = target as usize;
                }
            }
            Op::ChainEnd => {
                if let Some(extra) = self.frames[top].extra.as_mut() {
                    extra.chains.pop();
                }
            }
            Op::GetIterator(callsite) => {
                let value = self.pop();
                self.iterable_callsite = callsite.map(|index| {
                    Callsite::Source(self.frames[top].code.names[index as usize].clone())
                });
                let iterator = self.get_iterator(value);
                self.iterable_callsite = None;
                self.vm_stack.push(iterator?);
            }
            Op::Disposal(step) => {
                let value = match step {
                    super::ast::Disposal::Scope { asynchronous } => self.using_scope(asynchronous),
                    super::ast::Disposal::Use { asynchronous } => {
                        let value = self.pop();
                        let scope = self.pop();
                        self.using_add(&scope, value, asynchronous)?
                    }
                    super::ast::Disposal::End { error } => {
                        let thrown = error.then(|| self.pop());
                        let scope = self.pop();
                        self.using_end(&scope, thrown)?
                    }
                };
                self.vm_stack.push(value);
            }
            Op::ArgumentsIterator => {
                let Value::Array(array) = self.pop() else {
                    unreachable!("arguments arrive as an array")
                };
                self.vm_stack.push(Value::VmIterator(Rc::new(RefCell::new(
                    VmIterator::Array { array, index: 0 },
                ))));
            }
            Op::GetAsyncIterator => {
                let value = self.pop();
                let iterator = self.get_async_iterator(value)?;
                self.vm_stack.push(iterator);
            }
            Op::IterValue => {
                let iterator = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                let value = self.iterator_step(&iterator)?.unwrap_or(Value::Undefined);
                self.vm_stack.push(value);
            }
            Op::IterRest => {
                let iterator = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                let mut rest = Vec::new();
                while let Some(value) = self.iterator_step(&iterator)? {
                    rest.push(value);
                }
                self.vm_stack.push(Self::new_array(rest));
            }
            Op::IterClose => {
                let iterator = self.pop();
                self.iterator_close(&iterator)?;
            }
            Op::IterStep(target) => {
                let iterator = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                match self.iterator_step(&iterator)? {
                    Some(value) => self.vm_stack.push(value),
                    None => self.frames[top].pc = target as usize,
                }
            }
            Op::AsyncIterNext => {
                let iterator = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                let promise = self.async_iterator_next(&iterator)?;
                self.vm_stack.push(promise);
            }
            Op::IterResultUnpack(target) => {
                let result = self.pop();
                if !matches!(result, Value::Object(_)) {
                    return Err(self.type_error("iterator result is not an object"));
                }
                let done = self.get_property(&result, "done")?.truthy();
                if done {
                    self.frames[top].pc = target as usize;
                } else {
                    let value = self.get_property(&result, "value")?;
                    self.vm_stack.push(value);
                }
            }
            Op::ForInPrepare => {
                let value = self.pop();
                let keys = self.for_in_keys(&value)?;
                let iterator = Value::VmIterator(Rc::new(RefCell::new(VmIterator::Keys {
                    keys,
                    index: 0,
                    object: value,
                })));
                self.vm_stack.push(iterator);
            }
            Op::ForInNext(target) => {
                let iterator = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                match self.for_in_step(&iterator)? {
                    Some(key) => self.vm_stack.push(Value::String(key.into())),
                    None => self.frames[top].pc = target as usize,
                }
            }
            Op::Throw => {
                let value = self.pop();
                return Err(self.throw_value(value));
            }
            Op::ThrowConstAssign(name) => {
                let name = self.frames[top].code.names[name as usize].clone();
                return Err(self.type_error(format!("Assignment to constant variable '{name}'")));
            }
            Op::PushHandler { catch, finally } => {
                let stack_offset = self.vm_stack.len() - self.frames[top].stack_base;
                let scopes = self.frames[top].scopes;
                let extra = self.frames[top].extra_mut();
                let chains = extra.chains.len();
                extra.handlers.push(Handler {
                    catch,
                    finally,
                    stack: stack_offset,
                    scopes,
                    chains,
                });
            }
            Op::PopHandler => {
                if let Some(extra) = self.frames[top].extra.as_mut() {
                    extra.handlers.pop();
                }
            }
            Op::CompletionNormal => {
                self.frames[top].extra_mut().pending.push(Pending::Normal);
            }
            Op::EndFinally => {
                let pending = self.frames[top]
                    .extra
                    .as_mut()
                    .and_then(|extra| extra.pending.pop())
                    .unwrap_or(Pending::Normal);
                match pending {
                    Pending::Normal => {}
                    Pending::Throw(value) => return Err(self.throw_value(value)),
                    Pending::Return(value) => return self.do_return(frame_floor, value),
                    Pending::Jump {
                        target,
                        handlers,
                        scopes,
                    } => self.do_exit(target, handlers, scopes),
                }
            }
            Op::Exit {
                target,
                handlers,
                scopes,
            } => self.do_exit(target, handlers, scopes),
            Op::Return => {
                let value = self.pop();
                return self.do_return(frame_floor, value);
            }
            Op::Yield | Op::YieldDelegating => {
                let resume = self.frames[top]
                    .extra
                    .as_mut()
                    .and_then(|extra| extra.resume.take());
                if let Some((mode, value)) = resume {
                    // resuming: the sent value lands where the yield was
                    match (mode, matches!(op, Op::YieldDelegating)) {
                        (ResumeMode::Next, _) => self.vm_stack.push(value),
                        (mode, true) => {
                            self.frames[top].extra_mut().delegate_mode = mode;
                            self.vm_stack.push(value);
                        }
                        (ResumeMode::Throw, false) => {
                            return Err(self.throw_value(value));
                        }
                        (ResumeMode::Return, false) => return self.do_return(frame_floor, value),
                    }
                    return Ok(None);
                }
                let value = self.pop();
                // the yield runs again on resume to receive the sent value
                self.frames[top].pc -= 1;
                if self.frames.len() != frame_floor + 1 {
                    return Err(runtime_error("yield outside a resumable frame"));
                }
                let mut frame = self.frames.pop().expect("generator frame");
                let saved = self.vm_stack.split_off(frame.stack_base);
                truncate_stack(&mut self.vm_stack, frame.return_base);
                frame.extra_mut().saved_stack = saved;
                return Ok(Some(RunResult::Yield(value, Box::new(frame))));
            }
            Op::DelegateCall => {
                let sent = self.pop();
                let iterator = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                let extra = self.frames[top].extra_mut();
                let mode = std::mem::replace(&mut extra.delegate_mode, ResumeMode::Next);
                extra.delegate_last = mode;
                let promise = self.delegate_call(&iterator, mode, sent)?;
                self.vm_stack.push(promise);
            }
            Op::DelegateUnpack(target) => {
                let result = self.pop();
                if super::operations::is_primitive(&result) {
                    return Err(self.type_error("iterator result is not an object"));
                }
                let done = self.get_property(&result, "done")?.truthy();
                let value = self.get_property(&result, "value")?;
                if done {
                    self.pop().fast_drop();
                    let last = self.frames[top]
                        .extra
                        .as_ref()
                        .map_or(ResumeMode::Next, |extra| extra.delegate_last);
                    if last == ResumeMode::Return {
                        return self.do_return(frame_floor, value);
                    }
                    self.vm_stack.push(value);
                    self.frames[top].pc = target as usize;
                } else {
                    self.vm_stack.push(value);
                }
            }
            Op::DelegateStep(target) => {
                let sent = self.pop();
                let iterator = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                let mode = self.frames[top]
                    .extra
                    .as_mut()
                    .map_or(ResumeMode::Next, |extra| {
                        std::mem::replace(&mut extra.delegate_mode, ResumeMode::Next)
                    });
                let result = self.delegate_resume(&iterator, mode, sent)?;
                match result {
                    DelegateResult::Yielded(value) => self.vm_stack.push(value),
                    DelegateResult::Done(value) => {
                        self.pop().fast_drop();
                        self.vm_stack.push(value);
                        self.frames[top].pc = target as usize;
                    }
                    DelegateResult::Returned(value) => {
                        self.pop().fast_drop();
                        return self.do_return(frame_floor, value);
                    }
                }
            }
            Op::Await => {
                let resume = self.frames[top]
                    .extra
                    .as_mut()
                    .and_then(|extra| extra.resume.take());
                if let Some((mode, value)) = resume {
                    match mode {
                        ResumeMode::Next => self.vm_stack.push(value),
                        _ => {
                            return Err(self.throw_value(value));
                        }
                    }
                    return Ok(None);
                }
                let value = self.pop();
                self.frames[top].pc -= 1;
                if self.frames.len() != frame_floor + 1 {
                    return Err(runtime_error("await outside a resumable frame"));
                }
                let mut frame = self.frames.pop().expect("async frame");
                let saved = self.vm_stack.split_off(frame.stack_base);
                truncate_stack(&mut self.vm_stack, frame.return_base);
                frame.extra_mut().saved_stack = saved;
                return Ok(Some(RunResult::Await(value, Box::new(frame))));
            }
            Op::DynamicImport => {
                let options = self.pop();
                let specifier = self.pop();
                let url = self.frames[top]
                    .code
                    .url
                    .get()
                    .map(|url| (**url).clone())
                    .or_else(|| self.base_url.clone());
                let promise = self.dynamic_import_promise(url, specifier, options);
                self.vm_stack.push(promise);
            }
        }
        Ok(None)
    }

    fn set_home(function: &Value, home: &ObjectHandle) {
        if let Some(closure) = Self::closure_of(function) {
            closure.set_home_object(home.clone());
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
                    closure.insert_private_method(definition);
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
        let constructor = self.new_closure(template.constructor.clone(), scope);
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
            closure.set_home_object(prototype.clone());
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
        frame_floor: usize,
        arguments: Vec<Value>,
    ) -> Result<Option<RunResult>, ScriptError> {
        let top = self.frames.len() - 1;
        let function = self.frames[top].function_value(&self.vm_stack);
        let new_target = self.frames[top].new_target_value();
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
        if !matches!(self.frames[top].this, Value::Uninitialized) {
            return Err(self.reference_error("super constructor may only be called once"));
        }
        let this = self.construct(parent, arguments, Some(new_target))?;
        for frame in self.frames[frame_floor..].iter_mut().rev() {
            frame.this = this.clone();
            if !frame.is_eval() {
                break;
            }
        }
        if let Some(closure) = self.frames.last().and_then(|frame| frame.closure.clone()) {
            self.initialize_fields(&closure, &this)?;
        }
        self.vm_stack.push(this);
        Ok(None)
    }

    fn call_in_loop(
        &mut self,
        callee: Value,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Option<RunResult>, ScriptError> {
        match self.prepare_call(callee, this, arguments, None)? {
            Prepared::Done(value) => self.vm_stack.push(value),
            Prepared::Frame(frame) => {
                if self.frames.len() >= MAX_FRAMES {
                    return Err(self.range_error("Maximum call stack size exceeded"));
                }
                self.push_frame(frame);
            }
        }
        Ok(None)
    }

    fn construct_in_loop(
        &mut self,
        callee: Value,
        arguments: Vec<Value>,
        new_target: Option<Value>,
    ) -> Result<Option<RunResult>, ScriptError> {
        let new_target = new_target.unwrap_or_else(|| callee.clone());
        match self.prepare_construct(callee, arguments, new_target)? {
            Prepared::Done(value) => self.vm_stack.push(value),
            Prepared::Frame(frame) => {
                if self.frames.len() >= MAX_FRAMES {
                    return Err(self.range_error("Maximum call stack size exceeded"));
                }
                self.push_frame(frame);
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
        let started = frame.pc > frame.code.entry;
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
            frame.extra_mut().resume = Some((mode, value));
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
        let promise = new_promise(PromiseOutcome::Pending);
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
        if resume.is_some() {
            frame.extra_mut().resume = resume;
        }
        let promise = state.borrow().promise.clone();
        let root_base = self.temp_roots.len();
        self.temp_roots.push(Value::Promise(promise.clone()));
        let run_result = self.run(*frame);
        self.temp_roots.truncate(root_base);
        match run_result {
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
        let fulfilled = Value::Builtin(Builtin::native(Native::AsyncResume(
            AsyncResumeHandle(state.clone()),
            false,
        )));
        let rejected = Value::Builtin(Builtin::native(Native::AsyncResume(
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
        let promise = new_promise(PromiseOutcome::Pending);
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
            let started = frame.pc > frame.code.entry;
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
            self.async_generator_run(data, *frame, resume)?;
            if data.borrow().awaiting {
                return Ok(());
            }
        }
    }

    pub(super) fn async_generator_run(
        &mut self,
        data: &Rc<RefCell<AsyncGeneratorData>>,
        mut frame: Frame,
        resume: Option<(ResumeMode, Value)>,
    ) -> Result<(), ScriptError> {
        if resume.is_some() {
            frame.extra_mut().resume = resume;
        }
        let outcome = self.run(frame);
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
                let promise = new_promise(PromiseOutcome::Pending);
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
                self.async_generator_run(&data, *frame, Some((mode, value)))?;
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
    Frame(Frame),
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
        let extra = (!matches!(new_target, Value::Undefined) || !arguments.is_empty()).then(|| {
            Box::new(FrameExtra {
                new_target,
                arguments,
                ..FrameExtra::default()
            })
        });
        Frame {
            pc: code.entry,
            code,
            closure,
            reg_base: 0,
            stack_base: 0,
            return_base: 0,
            scope,
            this,
            function,
            scopes: 0,
            flags: 0,
            extra,
        }
    }
}

// what answers for a name ahead of its compiled binding
enum Shadow {
    With(Value),
    Grown(ScopeRef, usize),
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

#[cfg(test)]
#[test]
fn a_scope_keeps_no_room_for_eval_names_until_an_eval_adds_one() {
    assert_eq!(std::mem::size_of::<Scope>(), 48);
}
