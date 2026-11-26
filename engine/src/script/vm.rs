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
    #[inline(always)]
    pub(super) fn is_none(&self) -> bool {
        self.0.is_none()
    }

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

impl Drop for Scope {
    #[inline]
    fn drop(&mut self) {
        let len = self.slots.len();
        if len == 0 {
            return;
        }
        let ptr = self.slots.as_mut_ptr();
        // safety: clear len first so unwind cannot double-drop; skip drop_glue for Copy values
        unsafe {
            self.slots.set_len(0);
            for i in 0..len {
                let slot = ptr.add(i);
                if !(*slot).is_copy() {
                    drop_value_slow(std::ptr::read(slot));
                }
            }
        }
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
    // lexical this captured directly by arrows inside non-derived functions
    pub(super) lexical_this: Option<Value>,
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

    #[inline(always)]
    pub(super) fn lexical_this(&self) -> Option<Value> {
        let ptr = self.meta.get();
        if ptr.is_null() {
            return None;
        }
        // safety: non-null meta pointer is valid for self's lifetime
        unsafe { (*ptr).lexical_this.clone() }
    }

    pub(super) fn set_lexical_this(&self, this: Value) {
        self.with_meta_mut(|meta| meta.lexical_this = Some(this));
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
        if let Some(this) = &meta.lexical_this {
            tracer.trace_value(this);
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
    SuspendedStart(Box<Frame>),
    Suspended(Box<Frame>),
    Executing,
    Completed,
}

impl GeneratorState {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        if let Self::SuspendedStart(frame) | Self::Suspended(frame) = self {
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
pub(super) struct Handler {
    catch: u32,
    finally: u32,
    stack: usize,
    scopes: u16,
    chains: usize,
    iters: usize,
}

#[derive(Clone)]
pub(super) enum Pending {
    Normal,
    Throw(Value),
    Return(Value),
    Jump {
        target: u32,
        handlers: u16,
        scopes: u16,
    },
}

impl Pending {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        match self {
            Self::Throw(value) | Self::Return(value) => tracer.trace_value(value),
            Self::Normal | Self::Jump { .. } => {}
        }
    }
}

pub(super) const FLAG_USES_REGS: u8 = 1 << 0;
pub(super) const FLAG_OWNS_SCOPE: u8 = 1 << 1;
pub(super) const FLAG_CONSTRUCT: u8 = 1 << 2;
const FLAG_EVAL: u8 = 1 << 3;
pub(super) const FLAG_CALLEE_ON_STACK: u8 = 1 << 4;
pub(super) const FLAG_CALLBACK_REUSE: u8 = 1 << 5;

// rare per-frame state kept out of line so ordinary calls push zero heap allocations
pub(super) struct FrameExtra {
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
    pub(super) closure: Option<Rc<Closure>>,
    pub(super) pc: usize,
    pub(super) reg_base: usize,
    pub(super) stack_base: usize,
    pub(super) return_base: usize,
    // native JIT resume address in the caller frame when called from JIT, null otherwise
    pub(super) return_target: *const u8,
    pub(super) handler_base: u32,
    pub(super) chain_base: u32,
    pub(super) pending_base: u32,
    pub(super) iter_base: u32,
    pub(super) scope: Option<ScopeRef>,
    pub(super) this: Value,
    pub(super) function: Value,
    pub(super) scopes: u16,
    // number of nested scopes from depth 0 that may have escaped to a closure or eval
    pub(super) escaped_scopes: u16,
    pub(super) flags: u8,
    pub(super) extra: Option<Box<FrameExtra>>,
}

impl Frame {
    #[inline(always)]
    fn mark_scopes_escaped(&mut self) {
        let needed = self.scopes.saturating_add(1);
        if self.escaped_scopes < needed {
            self.escaped_scopes = needed;
        }
    }
    #[inline(always)]
    fn uses_regs(&self) -> bool {
        self.flags & FLAG_USES_REGS != 0
    }

    #[inline(always)]
    pub(super) fn construct(&self) -> bool {
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
    pub(super) fn catches(&self, handlers: &[Handler], end: usize) -> bool {
        let start = self.handler_base as usize;
        handlers
            .get(start..end)
            .is_some_and(|slice| slice.iter().any(|handler| handler.catch != NO_TARGET))
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

    // avoids out-of-line drop_glue::<Value> calls for this/function on every return;
    // returns Some(code) only when this frame held the final reference to Code
    #[inline(always)]
    fn fast_drop_keep_unique_code(mut self) -> Option<Rc<Code>> {
        let this = std::mem::replace(&mut self.this, Value::Undefined);
        let function = std::mem::replace(&mut self.function, Value::Undefined);
        this.fast_drop();
        function.fast_drop();
        let md = std::mem::ManuallyDrop::new(self);
        // safety: this/function are Undefined (Copy); drop remaining fields in place
        unsafe {
            std::ptr::drop_in_place(&md.closure as *const _ as *mut Option<Rc<Closure>>);
            std::ptr::drop_in_place(&md.scope as *const _ as *mut Option<ScopeRef>);
            std::ptr::drop_in_place(&md.extra as *const _ as *mut Option<Box<FrameExtra>>);
            let code = std::ptr::read(&md.code);
            if Rc::strong_count(&code) == 1 {
                Some(code)
            } else {
                drop(code);
                None
            }
        }
    }

    // drops a frame directly in place inside the Vec slot before shrinking frames.len()
    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline(always)]
    pub(super) unsafe fn drop_in_place_keep_unique_code(slot: *mut Self) -> Option<Rc<Code>> {
        unsafe {
            let this_ptr = std::ptr::addr_of_mut!((*slot).this);
            if !(*this_ptr).is_copy() {
                drop_value_slow(std::ptr::read(this_ptr));
            }
            let fn_ptr = std::ptr::addr_of_mut!((*slot).function);
            if !(*fn_ptr).is_copy() {
                drop_value_slow(std::ptr::read(fn_ptr));
            }
            std::ptr::drop_in_place(std::ptr::addr_of_mut!((*slot).closure));
            let extra_ptr = std::ptr::addr_of_mut!((*slot).extra);
            if (*extra_ptr).is_some() {
                std::ptr::drop_in_place(extra_ptr);
            }
            let code = std::ptr::read(std::ptr::addr_of!((*slot).code));
            if Rc::strong_count(&code) == 1 {
                Some(code)
            } else {
                drop(code);
                None
            }
        }
    }
}

#[inline(always)]
fn truncate_stack(stack: &mut Vec<Value>, new_len: usize) {
    let len = stack.len();
    if len <= new_len {
        return;
    }
    let ptr = stack.as_mut_ptr();
    // safety: shrinking len before dropping prevents double-drop on unwind; new_len..len are live
    unsafe {
        stack.set_len(new_len);
        for index in new_len..len {
            let slot = ptr.add(index);
            if !(*slot).is_copy() {
                drop_value_slow(std::ptr::read(slot));
            }
        }
    }
}

#[inline(always)]
fn reserve_headroom(stack: &mut Vec<Value>, need: usize) {
    if stack.capacity() - stack.len() < need {
        reserve_headroom_cold(stack, need);
    }
}

#[cold]
#[inline(never)]
fn reserve_headroom_cold(stack: &mut Vec<Value>, need: usize) {
    stack.reserve(need);
}

#[inline(always)]
unsafe fn push_unchecked(stack: &mut Vec<Value>, value: Value) {
    let len = stack.len();
    debug_assert!(len < stack.capacity());
    unsafe {
        stack.as_mut_ptr().add(len).write(value);
        stack.set_len(len + 1);
    }
}

#[inline(always)]
fn pop_fast(stack: &mut Vec<Value>) -> Value {
    let len = stack.len();
    if len == 0 {
        return Value::Undefined;
    }
    let next = len - 1;
    // safety: next < len so slot is initialized and uniquely owned once len shrinks
    unsafe {
        stack.set_len(next);
        std::ptr::read(stack.as_ptr().add(next))
    }
}

#[inline(always)]
fn fast_compare(op: super::ast::BinaryOperator, left: &Value, right: &Value) -> Option<bool> {
    use super::ast::BinaryOperator as B;
    match (left, right) {
        (Value::Number(l), Value::Number(r)) => Some(match op {
            B::Less => l < r,
            B::LessEqual => l <= r,
            B::Greater => l > r,
            B::GreaterEqual => l >= r,
            B::StrictEqual | B::Equal => l == r,
            B::StrictNotEqual | B::NotEqual => l != r,
            _ => return None,
        }),
        _ => match op {
            B::StrictEqual => Some(values_equal(left, right)),
            B::StrictNotEqual => Some(!values_equal(left, right)),
            B::Equal | B::NotEqual => {
                let eq = match (left, right) {
                    (Value::Undefined | Value::Null, Value::Undefined | Value::Null) => true,
                    (Value::Undefined | Value::Null, _) | (_, Value::Undefined | Value::Null) => {
                        false
                    }
                    (Value::String(l), Value::String(r)) => l == r,
                    (Value::Bool(l), Value::Bool(r)) => l == r,
                    (Value::Object(l), Value::Object(r)) => ObjectHandle::ptr_eq(l, r),
                    (Value::Array(l), Value::Array(r)) => super::heap::ArrayHandle::ptr_eq(l, r),
                    (Value::Element(l), Value::Element(r)) => Rc::ptr_eq(l, r),
                    _ => return None,
                };
                Some(if matches!(op, B::Equal) { eq } else { !eq })
            }
            B::Less | B::LessEqual | B::Greater | B::GreaterEqual => {
                if let (Value::String(l), Value::String(r)) = (left, right)
                    && l.is_ascii()
                    && r.is_ascii()
                {
                    let ls: &str = l;
                    let rs: &str = r;
                    return Some(match op {
                        B::Less => ls < rs,
                        B::LessEqual => ls <= rs,
                        B::Greater => ls > rs,
                        _ => ls >= rs,
                    });
                }
                None
            }
            _ => None,
        },
    }
}

pub(super) enum FastNativeCall {
    NotHandled,
    Done,
    FramePushed,
    // Function.prototype.call rewrote vm_stack to a CallMethod(new_count) for fallback
    FallbackMethod(usize),
}

pub(super) enum RunResult {
    Return(Value),
    Yield(Value, Box<Frame>),
    Await(Value, Box<Frame>),
}

pub(super) const MAX_FRAMES: usize = 10_000;
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
        if !own_prototype && let Some(template) = code.function_template.get() {
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
        if !own_prototype && let Some(template) = object.template() {
            let _ = code.function_template.set(template);
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

    // checks whether callee is an ordinary JS closure eligible for direct stack-frame entry
    pub(super) fn simple_closure(callee: &Value) -> Option<Rc<Closure>> {
        let Value::Object(object) = callee else {
            return None;
        };
        let borrowed = object.try_borrow().ok()?;
        let ObjectClass::Function(closure) = &borrowed.class else {
            return None;
        };
        let code = &closure.code;
        if code.simple_params
            && !code.reads_arguments
            && code.self_scope.is_none()
            && !code.is_generator
            && !code.is_async
            && !matches!(
                code.kind,
                FunctionKind::ClassConstructor | FunctionKind::DerivedConstructor
            )
            && (!code.uses_regs || code.linear_params)
        {
            Some(closure.clone())
        } else {
            None
        }
    }

    // runs a pre-checked JS closure directly on vm_stack without allocating Vec<Value> or FrameExtra
    pub(super) fn call_simple_closure(
        &mut self,
        closure: &Rc<Closure>,
        callee: &Value,
        this: Value,
        args: &[Value],
    ) -> Result<Value, ScriptError> {
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
        if self.frames.len() >= MAX_FRAMES {
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        let code = &closure.code;
        let this = if code.uses_lexical_this {
            closure.lexical_this().unwrap_or(Value::Undefined)
        } else if code.strict || code.kind == FunctionKind::Arrow {
            this
        } else if matches!(this, Value::Undefined | Value::Null) {
            Value::Window
        } else if super::operations::is_primitive(&this) {
            self.coerce_object(this)?
        } else {
            this
        };
        self.nested_runs += 1;
        let mirror_base = self.debugger.mirror.len();
        let frame_floor = self.frames.len();
        let stack_floor = self.vm_stack.len();
        let handler_floor = self.vm_handlers.len();
        let chain_floor = self.vm_chains.len();
        let pending_floor = self.vm_pending.len();
        let iter_floor = self.vm_iters.len();
        let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
        if code.uses_regs {
            let reg_count = code.registers as usize;
            let param_count = code.param_slots.len();
            reserve_headroom(&mut self.vm_stack, 1 + reg_count + code.max_pushes as usize);
            let keep = args.len().min(param_count);
            let arg_start = stack_floor + 1;
            // safety: headroom for 1 + reg_count slots was reserved above
            unsafe {
                let ptr = self.vm_stack.as_mut_ptr().add(stack_floor);
                ptr.write(callee.clone());
                for (i, value) in args.iter().take(keep).enumerate() {
                    ptr.add(1 + i).write(value.clone());
                }
                for i in keep..reg_count {
                    ptr.add(1 + i).write(Value::Undefined);
                }
                self.vm_stack.set_len(arg_start + reg_count);
            }
            self.frames.push(Frame {
                code: code.clone(),
                closure: Some(closure.clone()),
                pc: code.entry,
                reg_base: arg_start,
                stack_base: arg_start + reg_count,
                return_base: stack_floor,
                return_target: std::ptr::null(),
                handler_base: handler_floor as u32,
                chain_base: chain_floor as u32,
                pending_base: pending_floor as u32,
                iter_base: iter_floor as u32,
                scope: closure.scope.clone(),
                this,
                function: Value::Undefined,
                scopes: 0,
                escaped_scopes: 0,
                flags: FLAG_USES_REGS | FLAG_CALLEE_ON_STACK,
                extra: None,
            });
        } else {
            let mut slots = heap.alloc_scope_slots(code.layout.names.len(), false);
            for (position, slot) in code.param_slots.iter().enumerate() {
                if let Some(slot) = slot
                    && let Some(value) = args.get(position)
                {
                    let old = std::mem::replace(
                        unsafe { slots.get_unchecked_mut(*slot as usize) },
                        value.clone(),
                    );
                    old.fast_drop();
                }
            }
            let scope = heap.alloc_scope_fast(Scope {
                slots,
                parent: closure.scope.clone(),
                layout: code.layout.clone(),
                extra: Grown::default(),
            });
            reserve_headroom(&mut self.vm_stack, 1 + code.max_pushes as usize);
            unsafe { push_unchecked(&mut self.vm_stack, callee.clone()) };
            let arg_start = self.vm_stack.len();
            self.frames.push(Frame {
                code: code.clone(),
                closure: Some(closure.clone()),
                pc: code.entry,
                reg_base: arg_start,
                stack_base: arg_start,
                return_base: stack_floor,
                return_target: std::ptr::null(),
                handler_base: handler_floor as u32,
                chain_base: chain_floor as u32,
                pending_base: pending_floor as u32,
                iter_base: iter_floor as u32,
                scope: Some(scope),
                this,
                function: Value::Undefined,
                scopes: 0,
                escaped_scopes: 0,
                flags: FLAG_OWNS_SCOPE | FLAG_CALLEE_ON_STACK,
                extra: None,
            });
        }
        let result = self.run_frames(frame_floor);
        if result.is_err() {
            self.frames.truncate(frame_floor);
            truncate_stack(&mut self.vm_stack, stack_floor);
            self.vm_handlers.truncate(handler_floor);
            self.vm_chains.truncate(chain_floor);
            self.vm_pending.truncate(pending_floor);
            self.vm_iters.truncate(iter_floor);
        }
        debug_assert_eq!(self.frames.len(), frame_floor);
        debug_assert_eq!(self.vm_stack.len(), stack_floor);
        debug_assert_eq!(self.vm_handlers.len(), handler_floor);
        debug_assert_eq!(self.vm_chains.len(), chain_floor);
        debug_assert_eq!(self.vm_pending.len(), pending_floor);
        debug_assert_eq!(self.vm_iters.len(), iter_floor);
        self.debugger.mirror.truncate(mirror_base);
        self.nested_runs -= 1;
        if self.nested_runs == 0 {
            self.debug_task_ended();
        }
        match result? {
            RunResult::Return(value) => Ok(value),
            _ => Err(runtime_error("a plain call cannot suspend")),
        }
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(super) fn jit_callback_enter(&mut self, here: usize) -> bool {
        if self.debugger.active || self.frames.len() >= MAX_FRAMES {
            return false;
        }
        if self.nested_runs == 0 {
            self.stack_base = here;
        } else if self.nested_runs >= MAX_NESTED_RUNS
            || self.stack_base.saturating_sub(here) > NATIVE_STACK_BUDGET
        {
            return false;
        }
        self.nested_runs += 1;
        true
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(super) fn jit_callback_exit(&mut self, mirror_base: usize) {
        self.debugger.mirror.truncate(mirror_base);
        self.nested_runs -= 1;
        if self.nested_runs == 0 {
            self.debug_task_ended();
        }
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(super) fn jit_callback_safepoint(&self, sampling: bool) -> u64 {
        self.safepoint_target(sampling)
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn jit_callback_finish_slow(
        &mut self,
        frame_floor: usize,
        stack_floor: usize,
        handler_floor: usize,
        chain_floor: usize,
        pending_floor: usize,
        iter_floor: usize,
        err: Option<ScriptError>,
    ) -> Result<Value, ScriptError> {
        let outcome = match err {
            None => self.run_frames(frame_floor),
            Some(error) if self.interrupted => Err(error),
            Some(error) => {
                let thrown = self.take_thrown(error);
                self.note_thrown(&thrown);
                if self.debugger.active {
                    self.debug_thrown(&thrown);
                }
                match self.unwind(frame_floor, thrown) {
                    Ok(Some(res)) => Ok(res),
                    Ok(None) => self.run_frames(frame_floor),
                    Err(unhandled) => Err(unhandled),
                }
            }
        };
        if outcome.is_err() {
            self.frames.truncate(frame_floor);
            truncate_stack(&mut self.vm_stack, stack_floor);
            self.vm_handlers.truncate(handler_floor);
            self.vm_chains.truncate(chain_floor);
            self.vm_pending.truncate(pending_floor);
            self.vm_iters.truncate(iter_floor);
        }
        debug_assert_eq!(self.frames.len(), frame_floor);
        debug_assert_eq!(self.vm_stack.len(), stack_floor);
        debug_assert_eq!(self.vm_handlers.len(), handler_floor);
        debug_assert_eq!(self.vm_chains.len(), chain_floor);
        debug_assert_eq!(self.vm_pending.len(), pending_floor);
        debug_assert_eq!(self.vm_iters.len(), iter_floor);
        match outcome? {
            RunResult::Return(value) => Ok(value),
            _ => Err(runtime_error("a plain call cannot suspend")),
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
            enum CallKind {
                Function(Rc<Closure>),
                Bound(Box<super::object::BoundFunction>),
                Native(Rc<Native>),
                Proxy(Rc<super::reflect::ProxyData>),
                NotCallable,
            }
            let kind = match &object.borrow().class {
                ObjectClass::Function(closure) => CallKind::Function(closure.clone()),
                ObjectClass::Bound(bound) => CallKind::Bound(bound.clone()),
                ObjectClass::Native(native) => CallKind::Native(native.clone()),
                ObjectClass::Proxy(proxy) => CallKind::Proxy(proxy.clone()),
                _ => CallKind::NotCallable,
            };
            match kind {
                CallKind::Function(closure) => {
                    if matches!(
                        closure.code.kind,
                        FunctionKind::ClassConstructor | FunctionKind::DerivedConstructor
                    ) {
                        return Err(self.type_error(format!(
                            "class constructor {} cannot be invoked without 'new'",
                            closure.code.name
                        )));
                    }
                    let this = if closure.code.uses_lexical_this {
                        closure.lexical_this().unwrap_or(Value::Undefined)
                    } else if closure.code.strict || closure.code.kind == FunctionKind::Arrow {
                        this
                    } else if matches!(this, Value::Undefined | Value::Null) {
                        Value::Window
                    } else if super::operations::is_primitive(&this) {
                        self.coerce_object(this)?
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
                        let frame = if closure.code.simple_params {
                            frame
                        } else {
                            match self.run(frame)? {
                                RunResult::Yield(_, suspended) => *suspended,
                                RunResult::Return(_) | RunResult::Await(..) => {
                                    return Err(runtime_error(
                                        "generator parameter setup did not suspend",
                                    ));
                                }
                            }
                        };
                        return Ok(Prepared::Done(
                            self.make_generator(&callee, &closure, frame)?,
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
                CallKind::Bound(bound) => {
                    let super::object::BoundFunction {
                        target,
                        this,
                        arguments: bound,
                    } = *bound;
                    let mut all = bound;
                    all.extend(arguments);
                    return self.prepare_call(target, this, all, None);
                }
                CallKind::Native(native) => match native.as_ref() {
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
                        let args_list = arguments.get(2).unwrap_or(&Value::Undefined);
                        if super::operations::is_primitive(args_list) {
                            return Err(
                                self.type_error("CreateListFromArrayLike called on non-object")
                            );
                        }
                        let list = self.array_like_list(args_list)?;
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
                    other => {
                        return Ok(Prepared::Done(self.call_native(other, this, arguments)?));
                    }
                },
                CallKind::Proxy(proxy) => {
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
                CallKind::NotCallable => {
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
                        Some(object),
                    )?));
                }
                ObjectClass::Proxy(proxy) => {
                    if !proxy.constructor {
                        return Err(self.type_error("value is not a constructor"));
                    }
                    let (target, trap) = self.proxy_trap(&proxy, "construct")?;
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
        let default = self.intrinsics.object_prototype.clone();
        self.prototype_from_constructor_or(new_target, &default)
    }

    pub(super) fn prototype_from_constructor_or(
        &mut self,
        new_target: &Value,
        default: &ObjectHandle,
    ) -> Result<ObjectHandle, ScriptError> {
        match self.get_property(new_target, "prototype")? {
            Value::Object(prototype) => Ok(prototype),
            Value::Array(array) => Ok(self.array_prototype_wrapper(&array)),
            _ => Ok(default.clone()),
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
        let moves = !code.reads_arguments;
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
            {
                let mut scope = scope.borrow_mut();
                for (slot, kind) in scope.slots.iter_mut().zip(code.layout.kinds.iter()) {
                    if *kind == BindingKind::Param {
                        *slot = Value::Uninitialized;
                    }
                }
            }
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
            Value::Builtin(builtin) => match builtin.as_ref() {
                Builtin::Native(native) => self.call_native(native, this, arguments),
                _ => self.call_builtin(builtin, arguments),
            },
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
        frame.handler_base = self.vm_handlers.len() as u32;
        frame.chain_base = self.vm_chains.len() as u32;
        frame.pending_base = self.vm_pending.len() as u32;
        frame.iter_base = self.vm_iters.len() as u32;
        if let Some(extra) = frame.extra.as_mut() {
            if !extra.handlers.is_empty() {
                self.vm_handlers.append(&mut extra.handlers);
            }
            if !extra.chains.is_empty() {
                self.vm_chains.append(&mut extra.chains);
            }
            if !extra.pending.is_empty() {
                self.vm_pending.append(&mut extra.pending);
            }
        }
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
            if let Some(extra) = frame.extra.as_mut() {
                if !extra.saved_stack.is_empty() {
                    self.vm_stack.append(&mut extra.saved_stack);
                }
                if extra.is_empty() {
                    frame.extra = None;
                }
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
        let handler_floor = self.vm_handlers.len();
        let chain_floor = self.vm_chains.len();
        let pending_floor = self.vm_pending.len();
        let iter_floor = self.vm_iters.len();
        self.push_frame(frame);
        let result = self.run_frames(frame_floor);
        if result.is_err() {
            self.frames.truncate(frame_floor);
            truncate_stack(&mut self.vm_stack, stack_floor);
            self.vm_handlers.truncate(handler_floor);
            self.vm_chains.truncate(chain_floor);
            self.vm_pending.truncate(pending_floor);
            self.vm_iters.truncate(iter_floor);
        }
        debug_assert_eq!(self.frames.len(), frame_floor);
        debug_assert_eq!(self.vm_stack.len(), stack_floor);
        debug_assert_eq!(self.vm_handlers.len(), handler_floor);
        debug_assert_eq!(self.vm_chains.len(), chain_floor);
        debug_assert_eq!(self.vm_pending.len(), pending_floor);
        debug_assert_eq!(self.vm_iters.len(), iter_floor);
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
            let handler_base = self.frames[top].handler_base as usize;
            let handler = if self.vm_handlers.len() > handler_base {
                self.vm_handlers.pop()
            } else {
                None
            };
            if let Some(handler) = handler {
                let target_stack = self.frames[top].stack_base + handler.stack;
                truncate_stack(&mut self.vm_stack, target_stack);
                let frame = &mut self.frames[top];
                Self::restore_scopes(frame, handler.scopes);
                self.vm_chains
                    .truncate(frame.chain_base as usize + handler.chains);
                self.vm_iters
                    .truncate(frame.iter_base as usize + handler.iters);
                if handler.catch != NO_TARGET {
                    self.vm_stack.push(thrown);
                    frame.pc = handler.catch as usize;
                    self.note_caught();
                } else {
                    self.vm_pending.push(Pending::Throw(thrown));
                    frame.pc = handler.finally as usize;
                }
                return Ok(None);
            }
            if self.frames.len() == frame_floor + 1 {
                return Err(self.throw_value(thrown));
            }
            let finished = self.frames.pop().expect("frame");
            truncate_stack(&mut self.vm_stack, finished.return_base);
            self.vm_handlers.truncate(finished.handler_base as usize);
            self.vm_chains.truncate(finished.chain_base as usize);
            self.vm_pending.truncate(finished.pending_base as usize);
            self.vm_iters.truncate(finished.iter_base as usize);
        }
    }

    fn restore_scopes_fast(heap: &super::heap::Heap, frame: &mut Frame, target: u16) {
        while frame.scopes > target {
            let depth = frame.scopes;
            let popped = frame.scope.take();
            let parent = popped.as_ref().and_then(|scope| {
                unsafe { heap.scope_at_ref(scope, 0) }
                    .map(|s| s.parent.clone())
                    .unwrap_or_else(|| scope.borrow().parent.clone())
            });
            if depth >= frame.escaped_scopes
                && let Some(scope) = popped
            {
                heap.free_scope_fast(scope);
            }
            frame.scope = parent;
            frame.scopes -= 1;
            if frame.escaped_scopes > frame.scopes + 1 {
                frame.escaped_scopes = frame.scopes + 1;
            }
        }
    }

    fn restore_scopes(frame: &mut Frame, target: u16) {
        while frame.scopes > target {
            let depth = frame.scopes;
            let popped = frame.scope.take();
            let parent = popped
                .as_ref()
                .and_then(|scope| scope.borrow().parent.clone());
            if depth >= frame.escaped_scopes
                && let Some(scope) = popped
            {
                super::heap::free_scope(scope);
            }
            frame.scope = parent;
            frame.scopes -= 1;
            if frame.escaped_scopes > frame.scopes + 1 {
                frame.escaped_scopes = frame.scopes + 1;
            }
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

    fn shadowing_loc(
        &mut self,
        scope: Option<ScopeRef>,
        name: &str,
        depth: u16,
    ) -> Result<Option<ShadowLoc>, ScriptError> {
        let mut walk = scope;
        let mut level = 0u16;
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
                    return Ok(Some(ShadowLoc::With(object)));
                }
            } else if let Some(index) = grown {
                return Ok(Some(ShadowLoc::Grown(level, index as u16)));
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
        let key = self.frames[top].code.names[name as usize].clone();
        self.read_local_named(top, depth, index, &key)
    }

    fn read_local_named(
        &mut self,
        top: usize,
        depth: u16,
        index: u16,
        name: &str,
    ) -> Result<Value, ScriptError> {
        let value = Self::with_scope_at(&self.frames[top], depth, |scope| {
            match &scope.slots[index as usize] {
                Value::Binding(cell) | Value::ConstBinding(cell) => cell.borrow().clone(),
                value => value.clone(),
            }
        });
        if matches!(value, Value::Uninitialized) {
            return Err(self.reference_error(format!("cannot access {name} before initialization")));
        }
        Ok(value)
    }

    fn local_layout_name(&self, top: usize, depth: u16, index: u16) -> Name {
        Self::with_scope_at(&self.frames[top], depth, |scope| {
            scope
                .layout
                .names
                .get(index as usize)
                .cloned()
                .unwrap_or_else(|| "".into())
        })
    }

    fn write_local(
        &mut self,
        top: usize,
        depth: u16,
        index: u16,
        name: u32,
        value: Value,
    ) -> Result<(), ScriptError> {
        let key = self.frames[top].code.names[name as usize].clone();
        self.write_local_named(top, depth, index, &key, value)
    }

    fn write_local_named(
        &mut self,
        top: usize,
        depth: u16,
        index: u16,
        name: &str,
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
                return Err(self.type_error(format!("Assignment to constant variable '{name}'")));
            }
            Value::Binding(_) | Value::Uninitialized => {
                drop(scope);
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
        frame.mark_scopes_escaped();
        let caller_url = frame.code.url.get().cloned();
        let (this, new_target) = if frame.code.kind == FunctionKind::Arrow {
            (
                if frame.code.uses_lexical_this {
                    frame.this.clone()
                } else {
                    Self::lexical_value(frame, "%this").unwrap_or(Value::Window)
                },
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
        let handler_base = self.frames[top].handler_base as usize;
        while self.vm_handlers.len() > handler_base {
            let handler = self.vm_handlers.pop().expect("checked above");
            if handler.finally != NO_TARGET {
                let target_stack = self.frames[top].stack_base + handler.stack;
                truncate_stack(&mut self.vm_stack, target_stack);
                let frame = &mut self.frames[top];
                Self::restore_scopes(frame, handler.scopes);
                self.vm_chains
                    .truncate(frame.chain_base as usize + handler.chains);
                self.vm_iters
                    .truncate(frame.iter_base as usize + handler.iters);
                self.vm_pending.push(Pending::Return(value));
                frame.pc = handler.finally as usize;
                return Ok(None);
            }
        }
        let mut finished = self.frames.pop().expect("a frame is running");
        truncate_stack(&mut self.vm_stack, finished.return_base);
        self.vm_handlers.truncate(finished.handler_base as usize);
        self.vm_chains.truncate(finished.chain_base as usize);
        self.vm_pending.truncate(finished.pending_base as usize);
        self.vm_iters.truncate(finished.iter_base as usize);
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
    fn call_plain(&mut self, heap: &super::heap::Heap, count: usize, method: bool) -> bool {
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
            let Value::Object(object) = (unsafe { self.vm_stack.get_unchecked(callee_at) }) else {
                return false;
            };
            if let Some(borrowed) = unsafe { heap.object_ref(object) } {
                let ObjectClass::Function(closure) = &borrowed.class else {
                    return false;
                };
                closure.clone()
            } else {
                let Ok(borrowed) = object.try_borrow() else {
                    return false;
                };
                let ObjectClass::Function(closure) = &borrowed.class else {
                    return false;
                };
                closure.clone()
            }
        };
        let code = closure.code.clone();
        if code.reads_arguments
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
            std::mem::replace(
                unsafe { self.vm_stack.get_unchecked_mut(return_base) },
                Value::Undefined,
            )
        } else {
            Value::Undefined
        };
        let this = if code.uses_lexical_this {
            closure.lexical_this().unwrap_or(Value::Undefined)
        } else if code.strict || code.kind == FunctionKind::Arrow {
            this
        } else if matches!(this, Value::Undefined | Value::Null) {
            Value::Window
        } else if super::operations::is_primitive(&this) {
            match self.coerce_object(this) {
                Ok(object) => object,
                Err(_) => return false,
            }
        } else {
            this
        };
        let handler_base = self.vm_handlers.len() as u32;
        let chain_base = self.vm_chains.len() as u32;
        let pending_base = self.vm_pending.len() as u32;
        let iter_base = self.vm_iters.len() as u32;
        if code.uses_regs {
            let reg_count = code.registers as usize;
            let param_count = code.param_slots.len();
            if code.linear_params {
                let keep = count.min(param_count);
                if count > keep {
                    truncate_stack(&mut self.vm_stack, arg_start + keep);
                }
                let grow = reg_count.saturating_sub(keep);
                reserve_headroom(&mut self.vm_stack, grow + code.max_pushes as usize);
                if keep < reg_count {
                    // safety: headroom for grow slots was just reserved above
                    unsafe {
                        let ptr = self.vm_stack.as_mut_ptr().add(arg_start + keep);
                        for i in 0..grow {
                            ptr.add(i).write(Value::Undefined);
                        }
                        self.vm_stack.set_len(arg_start + reg_count);
                    }
                }
            } else {
                // duplicate parameter names in sloppy mode: last occurrence wins
                let mut args = self.vm_stack.split_off(arg_start);
                reserve_headroom(&mut self.vm_stack, reg_count + code.max_pushes as usize);
                unsafe {
                    let ptr = self.vm_stack.as_mut_ptr().add(arg_start);
                    for i in 0..reg_count {
                        ptr.add(i).write(Value::Undefined);
                    }
                    self.vm_stack.set_len(arg_start + reg_count);
                }
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
                return_target: std::ptr::null(),
                handler_base,
                chain_base,
                pending_base,
                iter_base,
                scope,
                this,
                function: Value::Undefined,
                scopes: 0,
                escaped_scopes: 0,
                flags: FLAG_USES_REGS | FLAG_CALLEE_ON_STACK,
                extra: None,
            });
            return true;
        }
        let mut slots = heap.alloc_scope_slots(code.layout.names.len(), false);
        if code.simple_params {
            // every parameter is written, the ones past the arguments as undefined, so the
            // last of two parameters with one name decides its value
            for (position, slot) in code.param_slots.iter().enumerate() {
                if let Some(slot) = slot {
                    let arg = if position < count {
                        std::mem::replace(
                            unsafe { self.vm_stack.get_unchecked_mut(arg_start + position) },
                            Value::Undefined,
                        )
                    } else {
                        Value::Undefined
                    };
                    let old =
                        std::mem::replace(unsafe { slots.get_unchecked_mut(*slot as usize) }, arg);
                    old.fast_drop();
                }
            }
            truncate_stack(&mut self.vm_stack, arg_start);
            reserve_headroom(&mut self.vm_stack, code.max_pushes as usize);
        } else {
            for (slot, kind) in slots.iter_mut().zip(code.layout.kinds.iter()) {
                if *kind == BindingKind::Param {
                    *slot = Value::Uninitialized;
                }
            }
            // non-simple parameter lists unpack arguments from an array pushed at stack_base
            let args_vec = if count == 0 {
                Vec::new()
            } else {
                self.vm_stack.split_off(arg_start)
            };
            let args_array = heap.alloc_array_fast(ArrayData::from(args_vec));
            reserve_headroom(&mut self.vm_stack, 1 + code.max_pushes as usize);
            unsafe { push_unchecked(&mut self.vm_stack, Value::Array(args_array)) };
        }
        let scope = heap.alloc_scope_fast(Scope {
            slots,
            parent: closure.scope.clone(),
            layout: code.layout.clone(),
            extra: Grown::default(),
        });
        let pc = code.entry;
        self.frames.push(Frame {
            code,
            closure: Some(closure),
            pc,
            reg_base: arg_start,
            stack_base: arg_start,
            return_base,
            return_target: std::ptr::null(),
            handler_base,
            chain_base,
            pending_base,
            iter_base,
            scope: Some(scope),
            this,
            function: Value::Undefined,
            scopes: 0,
            escaped_scopes: 0,
            flags: FLAG_OWNS_SCOPE | FLAG_CALLEE_ON_STACK,
            extra: None,
        });
        true
    }

    // zero-allocation fast paths for the hottest native calls directly from vm_stack
    fn call_fast_native(
        &mut self,
        heap: &super::heap::Heap,
        count: usize,
        method: bool,
    ) -> Result<FastNativeCall, ScriptError> {
        let Some(arg_start) = self.vm_stack.len().checked_sub(count) else {
            return Ok(FastNativeCall::NotHandled);
        };
        let Some(callee_at) = arg_start.checked_sub(1) else {
            return Ok(FastNativeCall::NotHandled);
        };
        let Some(return_base) = callee_at.checked_sub(usize::from(method)) else {
            return Ok(FastNativeCall::NotHandled);
        };
        enum HotNative {
            String(super::strings::StringMethod),
            ArrayPush,
            ArrayPop,
            ArrayCallback(super::arrays::ArrayMethod),
            ArrayIsArray,
            JsonParse,
            FunctionCall,
            Bound,
            Collection(super::collections::CollectionMethod),
            ObjectGetOwnPropertyDescriptor,
            Dom(super::interfaces::DomMember),
        }
        let hot = match unsafe { self.vm_stack.get_unchecked(callee_at) } {
            Value::Builtin(b) if matches!(b.as_ref(), Builtin::ArrayIsArray) => {
                HotNative::ArrayIsArray
            }
            Value::Object(object) => {
                let Some(borrowed) = (unsafe { heap.object_ref(object) }) else {
                    return Ok(FastNativeCall::NotHandled);
                };
                match &borrowed.class {
                    ObjectClass::Bound(bound) if bound.arguments.len() + count <= 16 => {
                        HotNative::Bound
                    }
                    ObjectClass::Native(native) => match native.as_ref() {
                        Native::String(m) if count <= 4 => HotNative::String(*m),
                        Native::Array(super::arrays::ArrayMethod::Push) if method => {
                            HotNative::ArrayPush
                        }
                        Native::Array(super::arrays::ArrayMethod::Pop) if method => {
                            HotNative::ArrayPop
                        }
                        Native::Array(
                            m @ (super::arrays::ArrayMethod::ForEach
                            | super::arrays::ArrayMethod::Map
                            | super::arrays::ArrayMethod::Filter
                            | super::arrays::ArrayMethod::Some
                            | super::arrays::ArrayMethod::Every
                            | super::arrays::ArrayMethod::Find
                            | super::arrays::ArrayMethod::FindIndex
                            | super::arrays::ArrayMethod::Reduce
                            | super::arrays::ArrayMethod::ReduceRight
                            | super::arrays::ArrayMethod::Sort
                            | super::arrays::ArrayMethod::ToSorted),
                        ) if method && count <= 2 => HotNative::ArrayCallback(*m),
                        Native::JsonParse if count <= 2 => HotNative::JsonParse,
                        Native::FunctionCall if method => HotNative::FunctionCall,
                        Native::Collection(
                            m @ (super::collections::CollectionMethod::Get(
                                super::collections::CollectionKind::Map,
                            )
                            | super::collections::CollectionMethod::Has(
                                super::collections::CollectionKind::Map
                                | super::collections::CollectionKind::Set,
                            )
                            | super::collections::CollectionMethod::Set(
                                super::collections::CollectionKind::Map,
                            )
                            | super::collections::CollectionMethod::Add(
                                super::collections::CollectionKind::Set,
                            )
                            | super::collections::CollectionMethod::Delete(
                                super::collections::CollectionKind::Map
                                | super::collections::CollectionKind::Set,
                            )
                            | super::collections::CollectionMethod::Clear(
                                super::collections::CollectionKind::Map
                                | super::collections::CollectionKind::Set,
                            )
                            | super::collections::CollectionMethod::Size(
                                super::collections::CollectionKind::Map
                                | super::collections::CollectionKind::Set,
                            )),
                        ) if method && count <= 2 => HotNative::Collection(*m),
                        Native::ObjectGetOwnPropertyDescriptor if count == 2 => {
                            HotNative::ObjectGetOwnPropertyDescriptor
                        }
                        Native::Dom(member)
                            if method
                                && count <= 4
                                && self.custom_definitions.is_empty()
                                && member.is_hot() =>
                        {
                            HotNative::Dom(*member)
                        }
                        _ => return Ok(FastNativeCall::NotHandled),
                    },
                    _ => return Ok(FastNativeCall::NotHandled),
                }
            }
            _ => return Ok(FastNativeCall::NotHandled),
        };
        match hot {
            HotNative::String(string_method) => {
                let mut args = [
                    Value::Undefined,
                    Value::Undefined,
                    Value::Undefined,
                    Value::Undefined,
                ];
                // safety: arg_start + count == vm_stack.len(), callee is Copy Value::Object
                let this = unsafe {
                    let base = self.vm_stack.as_ptr();
                    for (i, slot) in args.iter_mut().take(count).enumerate() {
                        *slot = std::ptr::read(base.add(arg_start + i));
                    }
                    let this = if method {
                        std::ptr::read(base.add(return_base))
                    } else {
                        Value::Undefined
                    };
                    self.vm_stack.set_len(return_base);
                    this
                };
                let result = self.call_string_method_slice(string_method, &this, &args[..count]);
                this.fast_drop();
                for slot in &mut args[..count] {
                    std::mem::replace(slot, Value::Undefined).fast_drop();
                }
                // every element of args is Undefined, so skip drop_in_place<[Value; 4]>
                std::mem::forget(args);
                self.vm_stack.push(result?);
                Ok(FastNativeCall::Done)
            }
            HotNative::Bound => {
                let Value::Object(bound_handle) =
                    (unsafe { self.vm_stack.get_unchecked(callee_at).clone() })
                else {
                    return Ok(FastNativeCall::NotHandled);
                };
                let (target, bound_this, bound_len) = {
                    let Some(borrowed) = (unsafe { heap.object_ref(&bound_handle) }) else {
                        return Ok(FastNativeCall::NotHandled);
                    };
                    let ObjectClass::Bound(bound) = &borrowed.class else {
                        return Ok(FastNativeCall::NotHandled);
                    };
                    (
                        bound.target.clone(),
                        bound.this.clone(),
                        bound.arguments.len(),
                    )
                };
                if bound_len == 0 {
                    // common zero-bound-arg case avoids initializing a 256-byte argument buffer
                    let extra_slots = usize::from(!method);
                    if extra_slots > 0 {
                        reserve_headroom(&mut self.vm_stack, extra_slots);
                    }
                    unsafe {
                        let ptr = self.vm_stack.as_mut_ptr();
                        if method {
                            std::ptr::read(ptr.add(return_base)).fast_drop();
                        } else if count > 0 {
                            std::ptr::copy(ptr.add(arg_start), ptr.add(return_base + 2), count);
                        }
                        ptr.add(return_base).write(bound_this);
                        ptr.add(return_base + 1).write(target);
                        self.vm_stack.set_len(return_base + 2 + count);
                    }
                    if self.call_plain(heap, count, true) {
                        return Ok(FastNativeCall::FramePushed);
                    }
                    return Ok(FastNativeCall::FallbackMethod(count));
                }
                let mut bound_args: [Value; 16] = [const { Value::Undefined }; 16];
                {
                    let Some(borrowed) = (unsafe { heap.object_ref(&bound_handle) }) else {
                        return Ok(FastNativeCall::NotHandled);
                    };
                    let ObjectClass::Bound(bound) = &borrowed.class else {
                        return Ok(FastNativeCall::NotHandled);
                    };
                    for (dst, src) in bound_args
                        .iter_mut()
                        .take(bound_len)
                        .zip(bound.arguments.iter())
                    {
                        *dst = src.clone();
                    }
                }
                let new_count = bound_len + count;
                // rewrite [..., (orig_this?), bound_fn, a0..aN] -> [..., bound_this, target, b0..bM, a0..aN]
                let extra_slots = bound_len + usize::from(!method);
                reserve_headroom(&mut self.vm_stack, extra_slots);
                unsafe {
                    let ptr = self.vm_stack.as_mut_ptr();
                    if method {
                        std::ptr::read(ptr.add(return_base)).fast_drop();
                    }
                    let new_arg_start = return_base + 2;
                    if count > 0 {
                        std::ptr::copy(
                            ptr.add(arg_start),
                            ptr.add(new_arg_start + bound_len),
                            count,
                        );
                    }
                    ptr.add(return_base).write(bound_this);
                    ptr.add(return_base + 1).write(target);
                    for (i, slot) in bound_args.iter_mut().take(bound_len).enumerate() {
                        ptr.add(new_arg_start + i)
                            .write(std::mem::replace(slot, Value::Undefined));
                    }
                    std::mem::forget(bound_args);
                    self.vm_stack.set_len(new_arg_start + new_count);
                }
                if self.call_plain(heap, new_count, true) {
                    return Ok(FastNativeCall::FramePushed);
                }
                Ok(FastNativeCall::FallbackMethod(new_count))
            }
            HotNative::ArrayIsArray => {
                let is_arr = if count == 0 {
                    Ok(false)
                } else {
                    match unsafe { self.vm_stack.get_unchecked(arg_start) } {
                        Value::Array(_) => Ok(true),
                        Value::Object(obj) => {
                            if unsafe { heap.object_ref(obj) }
                                .is_some_and(|o| !matches!(o.class, ObjectClass::Proxy(_)))
                            {
                                Ok(false)
                            } else {
                                let arg = unsafe { self.vm_stack.get_unchecked(arg_start).clone() };
                                self.is_array(&arg)
                            }
                        }
                        other
                            if other.is_copy()
                                || matches!(other, Value::String(_) | Value::Element(_)) =>
                        {
                            Ok(false)
                        }
                        _ => {
                            let arg = unsafe { self.vm_stack.get_unchecked(arg_start).clone() };
                            self.is_array(&arg)
                        }
                    }
                };
                truncate_stack(&mut self.vm_stack, return_base);
                self.vm_stack.push(Value::Bool(is_arr?));
                Ok(FastNativeCall::Done)
            }
            HotNative::Collection(col_method) => {
                use super::collections::{CollectionKind as CK, CollectionMethod as M};
                let Value::Object(obj) = (unsafe { self.vm_stack.get_unchecked(return_base) })
                else {
                    return Ok(FastNativeCall::NotHandled);
                };
                let Some(borrowed) = (unsafe { heap.object_ref(obj) }) else {
                    return Ok(FastNativeCall::NotHandled);
                };
                let collection = match (&borrowed.class, col_method) {
                    (
                        ObjectClass::Map(col),
                        M::Get(CK::Map)
                        | M::Has(CK::Map)
                        | M::Set(CK::Map)
                        | M::Delete(CK::Map)
                        | M::Clear(CK::Map)
                        | M::Size(CK::Map),
                    )
                    | (
                        ObjectClass::Set(col),
                        M::Has(CK::Set)
                        | M::Add(CK::Set)
                        | M::Delete(CK::Set)
                        | M::Clear(CK::Set)
                        | M::Size(CK::Set),
                    ) => col.clone(),
                    _ => return Ok(FastNativeCall::NotHandled),
                };
                if matches!(col_method, M::Set(_) | M::Add(_))
                    && collection.borrow().len() >= MAX_RUNTIME_ARRAY_LENGTH
                {
                    return Ok(FastNativeCall::NotHandled);
                }
                let result = match col_method {
                    M::Get(_) => {
                        let value = if count == 0 {
                            collection.borrow().get(&Value::Undefined)
                        } else {
                            let key = unsafe { self.vm_stack.get_unchecked(arg_start) };
                            collection.borrow().get(key)
                        }
                        .unwrap_or(Value::Undefined);
                        truncate_stack(&mut self.vm_stack, return_base);
                        value
                    }
                    M::Has(_) => {
                        let has = if count == 0 {
                            collection.borrow().has(&Value::Undefined)
                        } else {
                            let key = unsafe { self.vm_stack.get_unchecked(arg_start) };
                            collection.borrow().has(key)
                        };
                        truncate_stack(&mut self.vm_stack, return_base);
                        Value::Bool(has)
                    }
                    M::Size(_) => {
                        let len = collection.borrow().len() as f64;
                        truncate_stack(&mut self.vm_stack, return_base);
                        Value::Number(len)
                    }
                    M::Delete(_) => {
                        let deleted = if count == 0 {
                            collection.borrow_mut().delete(&Value::Undefined)
                        } else {
                            let key = unsafe { self.vm_stack.get_unchecked(arg_start) };
                            collection.borrow_mut().delete(key)
                        };
                        truncate_stack(&mut self.vm_stack, return_base);
                        Value::Bool(deleted)
                    }
                    M::Clear(_) => {
                        collection.borrow_mut().clear();
                        truncate_stack(&mut self.vm_stack, return_base);
                        Value::Undefined
                    }
                    M::Set(_) | M::Add(_) => {
                        // safety: this and callee are Copy Value::Object; move args out directly
                        let (this_val, key, val) = unsafe {
                            let ptr = self.vm_stack.as_ptr();
                            let this_val = std::ptr::read(ptr.add(return_base));
                            let key = if count >= 1 {
                                std::ptr::read(ptr.add(arg_start))
                            } else {
                                Value::Undefined
                            };
                            let val = if matches!(col_method, M::Set(_)) && count >= 2 {
                                std::ptr::read(ptr.add(arg_start + 1))
                            } else {
                                if count >= 2 {
                                    std::ptr::read(ptr.add(arg_start + 1)).fast_drop();
                                }
                                Value::Undefined
                            };
                            self.vm_stack.set_len(return_base);
                            (this_val, key, val)
                        };
                        collection.borrow_mut().set(key, val);
                        this_val
                    }
                    _ => unreachable!(),
                };
                self.vm_stack.push(result);
                Ok(FastNativeCall::Done)
            }
            HotNative::ObjectGetOwnPropertyDescriptor => {
                let (Value::Object(obj), Value::String(key)) =
                    (unsafe { self.vm_stack.get_unchecked(arg_start) }, unsafe {
                        self.vm_stack.get_unchecked(arg_start + 1)
                    })
                else {
                    return Ok(FastNativeCall::NotHandled);
                };
                let Some(borrowed) = (unsafe { heap.object_ref(obj) }) else {
                    return Ok(FastNativeCall::NotHandled);
                };
                if matches!(
                    borrowed.class,
                    ObjectClass::Proxy(_) | ObjectClass::String(_)
                ) {
                    return Ok(FastNativeCall::NotHandled);
                }
                let found = borrowed.property(key).map(|prop| prop.to_property());
                truncate_stack(&mut self.vm_stack, return_base);
                let result = match found {
                    Some(property) => self.descriptor_value(&property),
                    None => Value::Undefined,
                };
                self.vm_stack.push(result);
                Ok(FastNativeCall::Done)
            }
            HotNative::Dom(member) => {
                let this_ok = matches!(
                    unsafe { self.vm_stack.get_unchecked(return_base) },
                    Value::Element(_) | Value::Document
                );
                if !this_ok {
                    return Ok(FastNativeCall::NotHandled);
                }
                let mut args = [
                    Value::Undefined,
                    Value::Undefined,
                    Value::Undefined,
                    Value::Undefined,
                ];
                // safety: arg_start + count == vm_stack.len(), callee is Copy Value::Object
                let this = unsafe {
                    let base = self.vm_stack.as_ptr();
                    for (i, slot) in args.iter_mut().take(count).enumerate() {
                        *slot = std::ptr::read(base.add(arg_start + i));
                    }
                    let this = std::ptr::read(base.add(return_base));
                    self.vm_stack.set_len(return_base);
                    this
                };
                let outcome = self.call_dom_hot(member, &this, &args[..count]);
                match outcome {
                    Ok(Some(value)) => {
                        for slot in &mut args[..count] {
                            std::mem::replace(slot, Value::Undefined).fast_drop();
                        }
                        std::mem::forget(args);
                        this.fast_drop();
                        self.vm_stack.push(value);
                        Ok(FastNativeCall::Done)
                    }
                    Ok(None) => {
                        let mut arguments = Vec::with_capacity(count);
                        for slot in &mut args[..count] {
                            arguments.push(std::mem::replace(slot, Value::Undefined));
                        }
                        std::mem::forget(args);
                        let value = self.call_dom(member, this, arguments)?;
                        self.vm_stack.push(value);
                        Ok(FastNativeCall::Done)
                    }
                    Err(error) => {
                        for slot in &mut args[..count] {
                            std::mem::replace(slot, Value::Undefined).fast_drop();
                        }
                        std::mem::forget(args);
                        this.fast_drop();
                        Err(error)
                    }
                }
            }
            HotNative::ArrayPush => {
                let Value::Array(array_handle) =
                    (unsafe { self.vm_stack.get_unchecked(return_base) })
                else {
                    return Ok(FastNativeCall::NotHandled);
                };
                let array_handle = array_handle.clone();
                let Some(array) = (unsafe { heap.array_mut(&array_handle) }) else {
                    return Ok(FastNativeCall::NotHandled);
                };
                if array.extra.is_some()
                    || !self.array_protos_clean(heap)
                    || array.values.len().saturating_add(count) > MAX_RUNTIME_ARRAY_LENGTH
                {
                    return Ok(FastNativeCall::NotHandled);
                }
                let needed = array.values.len() + count;
                if needed <= 2 && array.values.capacity() < needed {
                    array.values.reserve_exact(count);
                } else if count > 0 {
                    array.values.reserve(count);
                }
                // safety: moving count values from vm_stack[arg_start..]; this and callee are Copy
                unsafe {
                    let src = self.vm_stack.as_ptr().add(arg_start);
                    for i in 0..count {
                        array.values.push(std::ptr::read(src.add(i)));
                    }
                    self.vm_stack.set_len(return_base);
                }
                let new_len = array.values.len();
                self.vm_stack.push(Value::Number(new_len as f64));
                Ok(FastNativeCall::Done)
            }
            HotNative::ArrayPop => {
                let Value::Array(array_handle) =
                    (unsafe { self.vm_stack.get_unchecked(return_base) })
                else {
                    return Ok(FastNativeCall::NotHandled);
                };
                let array_handle = array_handle.clone();
                let Some(array) = (unsafe { heap.array_mut(&array_handle) }) else {
                    return Ok(FastNativeCall::NotHandled);
                };
                if array.extra.is_some() || !self.array_protos_clean(heap) {
                    return Ok(FastNativeCall::NotHandled);
                }
                let value = array.values.pop().unwrap_or(Value::Undefined).filled();
                truncate_stack(&mut self.vm_stack, return_base);
                self.vm_stack.push(value);
                Ok(FastNativeCall::Done)
            }
            HotNative::ArrayCallback(array_method) => {
                let mut args = [Value::Undefined, Value::Undefined];
                // safety: method is true, count <= 2, callee is Copy Value::Object
                let this = unsafe {
                    let base = self.vm_stack.as_ptr();
                    for (i, slot) in args.iter_mut().take(count).enumerate() {
                        *slot = std::ptr::read(base.add(arg_start + i));
                    }
                    let this = std::ptr::read(base.add(return_base));
                    self.vm_stack.set_len(return_base);
                    this
                };
                let result = self.call_array_callback_slice(array_method, this, &args[..count]);
                for slot in &mut args[..count] {
                    std::mem::replace(slot, Value::Undefined).fast_drop();
                }
                std::mem::forget(args);
                self.vm_stack.push(result?);
                Ok(FastNativeCall::Done)
            }
            HotNative::JsonParse => {
                // safety: count <= 2, callee is Copy Value::Object
                let (text, reviver) = unsafe {
                    let base = self.vm_stack.as_ptr();
                    if method {
                        std::ptr::read(base.add(return_base)).fast_drop();
                    }
                    let text = if count >= 1 {
                        std::ptr::read(base.add(arg_start))
                    } else {
                        Value::Undefined
                    };
                    let reviver = if count >= 2 {
                        std::ptr::read(base.add(arg_start + 1))
                    } else {
                        Value::Undefined
                    };
                    self.vm_stack.set_len(return_base);
                    (text, reviver)
                };
                let value = self.json_parse(text, reviver)?;
                self.vm_stack.push(value);
                Ok(FastNativeCall::Done)
            }
            HotNative::FunctionCall => {
                let is_fn = match unsafe { self.vm_stack.get_unchecked(return_base) } {
                    Value::Object(target) => unsafe { heap.object_ref(target) }
                        .is_some_and(|obj| matches!(obj.class, ObjectClass::Function(_))),
                    _ => false,
                };
                if !is_fn {
                    return Ok(FastNativeCall::NotHandled);
                }
                if count == 0 {
                    // safety: both target and call_fn are Copy Value::Object
                    unsafe {
                        let ptr = self.vm_stack.as_mut_ptr();
                        let target = std::ptr::read(ptr.add(return_base));
                        ptr.add(callee_at).write(target);
                        ptr.add(return_base).write(Value::Undefined);
                    }
                    if self.call_plain(heap, 0, true) {
                        return Ok(FastNativeCall::FramePushed);
                    }
                    return Ok(FastNativeCall::FallbackMethod(0));
                }
                let new_count = count - 1;
                // safety: rewrite [target, call_fn, new_this, a1..] -> [new_this, target, a1..]
                unsafe {
                    let ptr = self.vm_stack.as_mut_ptr();
                    let target = std::ptr::read(ptr.add(return_base));
                    let new_this = std::ptr::read(ptr.add(arg_start));
                    ptr.add(return_base).write(new_this);
                    ptr.add(callee_at).write(target);
                    if new_count > 0 {
                        std::ptr::copy(ptr.add(arg_start + 1), ptr.add(arg_start), new_count);
                    }
                    self.vm_stack.set_len(arg_start + new_count);
                }
                if self.call_plain(heap, new_count, true) {
                    return Ok(FastNativeCall::FramePushed);
                }
                Ok(FastNativeCall::FallbackMethod(new_count))
            }
        }
    }

    #[inline]
    fn call_ic_getter(&mut self, heap: &super::heap::Heap) -> Result<bool, ScriptError> {
        if self.call_plain(heap, 0, true) {
            return Ok(true);
        }
        match self.call_fast_native(heap, 0, true)? {
            FastNativeCall::Done => Ok(false),
            FastNativeCall::FramePushed => Ok(true),
            FastNativeCall::FallbackMethod(new_count) => {
                let arguments = self.pop_arguments(new_count);
                let callee = pop_fast(&mut self.vm_stack);
                let this = pop_fast(&mut self.vm_stack);
                let before = self.frames.len();
                self.call_in_loop(callee, this, arguments)?;
                Ok(self.frames.len() != before)
            }
            FastNativeCall::NotHandled => {
                let callee = pop_fast(&mut self.vm_stack);
                let this = pop_fast(&mut self.vm_stack);
                let before = self.frames.len();
                self.call_in_loop(callee, this, Vec::new())?;
                Ok(self.frames.len() != before)
            }
        }
    }

    #[inline(always)]
    fn ic_get_value(
        &self,
        heap: &super::heap::Heap,
        receiver: &Value,
        cache: &super::object::PropIc,
    ) -> Option<super::object::IcGetHit> {
        use super::object::IcGetHit;
        match receiver {
            Value::Object(handle) => unsafe { heap.object_ref(handle) }
                .and_then(|object| object.ic_get_fast(heap, cache)),
            Value::Element(handle) => unsafe { heap.object_ref(&handle.object) }
                .and_then(|object| object.ic_get_fast(heap, cache)),
            Value::Array(array) => {
                if cache.is_array_length() {
                    unsafe { heap.array_ref(array) }
                        .map(|a| IcGetHit::Value(Value::Number(a.len() as f64)))
                } else if unsafe { heap.array_ref(array) }.is_some_and(|a| a.extra.is_none()) {
                    JsObject::ic_array_get_fast(heap, &self.intrinsics.array_prototype, cache)
                        .map(IcGetHit::Value)
                } else {
                    None
                }
            }
            Value::String(text) => {
                if cache.is_string_length() {
                    Some(IcGetHit::Value(Value::Number(
                        super::strings::utf16_length(text) as f64,
                    )))
                } else {
                    JsObject::ic_string_get_fast(heap, &self.intrinsics.string_prototype, cache)
                        .map(IcGetHit::Value)
                }
            }
            _ => None,
        }
    }

    #[inline(always)]
    fn try_fast_get_index(heap: &super::heap::Heap, object: &Value, key: &Value) -> Option<Value> {
        match (object, key) {
            (Value::Array(array), Value::Number(number))
                if *number >= 0.0 && number.fract() == 0.0 =>
            {
                let idx = *number as usize;
                let data = unsafe { heap.array_ref(array)? };
                let value = data.values.get(idx)?;
                if matches!(value, Value::Uninitialized) {
                    return None;
                }
                Some(value.clone())
            }
            (Value::Object(obj), Value::String(key)) => {
                let data = unsafe { heap.object_ref(obj)? };
                data.own_data_or_missing(heap, key)
            }
            (Value::String(text), Value::Number(number))
                if text.is_ascii() && *number >= 0.0 && number.fract() == 0.0 =>
            {
                let idx = *number as usize;
                if idx < text.len() {
                    Some(Value::String(JsStr::new(&text[idx..idx + 1])))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    #[inline(always)]
    fn try_fast_set_index(
        heap: &super::heap::Heap,
        object: &Value,
        key: &Value,
        value: Value,
    ) -> Result<(), Value> {
        match (object, key) {
            (Value::Array(array), Value::Number(number))
                if *number >= 0.0 && number.fract() == 0.0 && *number < 4_294_967_295.0 =>
            {
                let idx = *number as usize;
                let Some(data) = (unsafe { heap.array_mut(array) }) else {
                    return Err(value);
                };
                if data.extra.is_some() {
                    return Err(value);
                }
                let len = data.values.len();
                if idx < len {
                    let old = std::mem::replace(&mut data.values[idx], value);
                    old.fast_drop();
                    return Ok(());
                }
                if idx == len && len < MAX_RUNTIME_ARRAY_LENGTH {
                    if len < 2 && data.values.capacity() <= len {
                        data.values.reserve_exact(1);
                    }
                    data.values.push(value);
                    return Ok(());
                }
                Err(value)
            }
            (Value::Object(obj), Value::String(key)) => {
                let Some(data) = (unsafe { heap.object_mut(obj) }) else {
                    return Err(value);
                };
                data.set_own_plain_data_or_add(obj, heap, key, value)
            }
            _ => Err(value),
        }
    }

    // leaves try regions down to a depth, running each finally block before the jump lands
    fn do_exit(&mut self, target: u32, handlers: u16, scopes: u16) {
        let top = self.frames.len() - 1;
        let stop_at = self.frames[top].handler_base as usize + handlers as usize;
        while self.vm_handlers.len() > stop_at {
            let handler = self.vm_handlers.pop().expect("checked above");
            if handler.finally != NO_TARGET {
                let target_stack = self.frames[top].stack_base + handler.stack;
                truncate_stack(&mut self.vm_stack, target_stack);
                let frame = &mut self.frames[top];
                Self::restore_scopes(frame, handler.scopes);
                self.vm_chains
                    .truncate(frame.chain_base as usize + handler.chains);
                self.vm_iters
                    .truncate(frame.iter_base as usize + handler.iters);
                self.vm_pending.push(Pending::Jump {
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

    // unified hot interpreter loop: keeps the active frame's code, pc, ops pointer, register
    // base and heap pointer in locals, stays inline across calls and returns, and checks
    // safepoints and stack headroom only at backward jumps and frame transitions
    fn run_fast(&mut self, frame_floor: usize, sampling: bool) -> Result<RunResult, ScriptError> {
        let heap_ptr: *const super::heap::Heap = super::heap::heap_ptr(self.heap_id);
        let mut top = self.frames.len() - 1;
        let mut code: *const Code = Rc::as_ptr(&self.frames[top].code);
        let mut ops_ptr: *const Op = unsafe { (&*code).ops.as_ptr() };
        let mut pc: usize = self.frames[top].pc;
        let mut reg_base: usize = self.frames[top].reg_base;
        let mut block_start: usize = pc;
        let mut safepoint_at: u64 = self.safepoint_target(sampling);
        reserve_headroom(&mut self.vm_stack, unsafe { (&*code).max_pushes as usize });
        #[cfg(all(
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        let mut try_jit = true;
        macro_rules! reload_frame {
            () => {
                top = self.frames.len() - 1;
                let frame = unsafe { self.frames.get_unchecked(top) };
                code = Rc::as_ptr(&frame.code);
                ops_ptr = unsafe { (&*code).ops.as_ptr() };
                pc = frame.pc;
                reg_base = frame.reg_base;
                block_start = pc;
                reserve_headroom(&mut self.vm_stack, unsafe { (&*code).max_pushes as usize });
                #[cfg(all(
                    target_arch = "x86_64",
                    any(target_os = "windows", target_os = "linux")
                ))]
                {
                    try_jit = true;
                }
            };
        }
        loop {
            #[cfg(all(
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            if try_jit {
                try_jit = false;
                loop {
                    let code_ref = unsafe { &*code };
                    let jit_opt = if let Some(cached) = code_ref.jit_code.get() {
                        cached.as_ref()
                    } else {
                        let hits = code_ref.jit_hits.get().wrapping_add(1);
                        code_ref.jit_hits.set(hits);
                        if hits >= super::jit::JIT_THRESHOLD {
                            let compiled = super::jit::compile(unsafe { &*heap_ptr }, code_ref);
                            let _ = code_ref.jit_code.set(compiled);
                            code_ref.jit_code.get().and_then(Option::as_ref)
                        } else {
                            None
                        }
                    };
                    let Some(jit) = jit_opt else {
                        break;
                    };
                    let Some(target) = jit.try_target(pc) else {
                        break;
                    };
                    // keep Rc<Code> (and its executable JitCode buffer) alive even if Op::Return
                    // pops the last frame referencing this Code before the JIT epilogue runs
                    let _keep_code = unsafe { self.frames.get_unchecked(top) }.code.clone();
                    match super::jit::run_jit_frame(
                        self,
                        jit,
                        target,
                        heap_ptr,
                        top,
                        frame_floor,
                        sampling,
                        &mut safepoint_at,
                    ) {
                        super::jit::JitOutcome::FrameChanged => {
                            top = self.frames.len() - 1;
                            let frame = unsafe { self.frames.get_unchecked(top) };
                            code = Rc::as_ptr(&frame.code);
                            ops_ptr = unsafe { (&*code).ops.as_ptr() };
                            pc = frame.pc;
                            reg_base = frame.reg_base;
                            block_start = pc;
                            reserve_headroom(&mut self.vm_stack, unsafe {
                                (&*code).max_pushes as usize
                            });
                        }
                        super::jit::JitOutcome::Done(result) => return Ok(result),
                        super::jit::JitOutcome::Error(error) => return Err(error),
                    }
                }
            }
            // safety: self.frames[top] owns Rc<Code> until frames changes, at which point we reload
            let op_pc = pc;
            debug_assert!(op_pc < unsafe { (&*code).ops.len() });
            let op = unsafe { *ops_ptr.add(op_pc) };
            pc = op_pc + 1;
            match op {
                Op::Nop => {}
                Op::Undefined => unsafe { push_unchecked(&mut self.vm_stack, Value::Undefined) },
                Op::Null => unsafe { push_unchecked(&mut self.vm_stack, Value::Null) },
                Op::True => unsafe { push_unchecked(&mut self.vm_stack, Value::Bool(true)) },
                Op::False => unsafe { push_unchecked(&mut self.vm_stack, Value::Bool(false)) },
                Op::Number(value) => unsafe {
                    push_unchecked(&mut self.vm_stack, Value::Number(value));
                },
                Op::String(index) => {
                    let text = unsafe { (&*code).names.get_unchecked(index as usize).clone() };
                    unsafe { push_unchecked(&mut self.vm_stack, Value::String(text)) };
                }
                Op::Pop => {
                    let len = self.vm_stack.len();
                    if len > 0 {
                        let next = len - 1;
                        unsafe {
                            self.vm_stack.set_len(next);
                            let slot = self.vm_stack.as_ptr().add(next);
                            if !(*slot).is_copy() {
                                drop_value_slow(std::ptr::read(slot));
                            }
                        }
                    }
                }
                Op::Dup => {
                    let len = self.vm_stack.len();
                    let copy = if len > 0 {
                        unsafe { self.vm_stack.get_unchecked(len - 1).clone() }
                    } else {
                        Value::Undefined
                    };
                    unsafe { push_unchecked(&mut self.vm_stack, copy) };
                }
                Op::Dup2 => {
                    let len = self.vm_stack.len();
                    let a = unsafe { self.vm_stack.get_unchecked(len - 2).clone() };
                    let b = unsafe { self.vm_stack.get_unchecked(len - 1).clone() };
                    unsafe {
                        push_unchecked(&mut self.vm_stack, a);
                        push_unchecked(&mut self.vm_stack, b);
                    }
                }
                Op::Swap => {
                    let length = self.vm_stack.len();
                    self.vm_stack.swap(length - 1, length - 2);
                }
                Op::GetReg { reg, name } => {
                    let value = unsafe { self.vm_stack.get_unchecked(reg_base + reg as usize) };
                    if matches!(value, Value::Uninitialized) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let name = unsafe { (&*code).names[name as usize].clone() };
                        return Err(self.reference_error(format!(
                            "cannot access {name} before initialization"
                        )));
                    }
                    let copy = value.clone();
                    unsafe { push_unchecked(&mut self.vm_stack, copy) };
                }
                Op::SetReg { reg, name } => {
                    let slot_index = reg_base + reg as usize;
                    if matches!(
                        unsafe { self.vm_stack.get_unchecked(slot_index) },
                        Value::Uninitialized
                    ) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let name = unsafe { (&*code).names[name as usize].clone() };
                        return Err(self.reference_error(format!(
                            "cannot access {name} before initialization"
                        )));
                    }
                    let len = self.vm_stack.len();
                    let value = if len > 0 {
                        unsafe { self.vm_stack.get_unchecked(len - 1).clone() }
                    } else {
                        Value::Undefined
                    };
                    let old = std::mem::replace(
                        unsafe { self.vm_stack.get_unchecked_mut(slot_index) },
                        value,
                    );
                    old.fast_drop();
                }
                Op::SetRegPop { reg } => {
                    let slot_index = reg_base + reg as usize;
                    if matches!(
                        unsafe { self.vm_stack.get_unchecked(slot_index) },
                        Value::Uninitialized
                    ) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let name = unsafe { (&*code).layout.names[reg as usize].clone() };
                        return Err(self.reference_error(format!(
                            "cannot access {name} before initialization"
                        )));
                    }
                    let value = pop_fast(&mut self.vm_stack);
                    let old = std::mem::replace(
                        unsafe { self.vm_stack.get_unchecked_mut(slot_index) },
                        value,
                    );
                    old.fast_drop();
                }
                Op::InitReg(reg) => {
                    let value = pop_fast(&mut self.vm_stack);
                    let old = std::mem::replace(
                        unsafe { self.vm_stack.get_unchecked_mut(reg_base + reg as usize) },
                        value,
                    );
                    old.fast_drop();
                }
                Op::UpdateReg {
                    reg,
                    name,
                    increment,
                    prefix,
                } => {
                    let slot_index = reg_base + reg as usize;
                    if let Value::Number(slot) =
                        unsafe { self.vm_stack.get_unchecked_mut(slot_index) }
                    {
                        let old = *slot;
                        let new = if increment { old + 1.0 } else { old - 1.0 };
                        *slot = new;
                        unsafe {
                            push_unchecked(
                                &mut self.vm_stack,
                                Value::Number(if prefix { new } else { old }),
                            );
                        }
                    } else {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        self.update_reg_slow(top, reg, name, increment, prefix)?;
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                    }
                }
                Op::UpdateRegPop { reg, increment } => {
                    let slot_index = reg_base + reg as usize;
                    if let Value::Number(slot) =
                        unsafe { self.vm_stack.get_unchecked_mut(slot_index) }
                    {
                        *slot = if increment { *slot + 1.0 } else { *slot - 1.0 };
                    } else {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let name = unsafe { (&*code).layout.names[reg as usize].clone() };
                        self.update_reg_slow_named(top, reg, &name, increment, false)?;
                        pop_fast(&mut self.vm_stack).fast_drop();
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
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
                    let fast = unsafe {
                        self.frames
                            .get_unchecked(top)
                            .scope
                            .as_ref()
                            .and_then(|start| (&*heap_ptr).scope_at_ref(start, depth))
                            .and_then(|scope| match scope.slots.get(index as usize) {
                                Some(Value::Binding(cell) | Value::ConstBinding(cell)) => {
                                    let inner = cell.borrow();
                                    (!matches!(*inner, Value::Uninitialized)).then(|| inner.clone())
                                }
                                Some(Value::Uninitialized) | None => None,
                                Some(value) => Some(value.clone()),
                            })
                    };
                    match fast {
                        Some(value) => unsafe { push_unchecked(&mut self.vm_stack, value) },
                        None => {
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            let value = self.read_local(top, depth, index, name)?;
                            unsafe { push_unchecked(&mut self.vm_stack, value) };
                        }
                    }
                }
                Op::SetLocal { depth, index, name } => {
                    let len = self.vm_stack.len();
                    let old_value = unsafe {
                        self.frames
                            .get_unchecked(top)
                            .scope
                            .as_ref()
                            .and_then(|start| (&*heap_ptr).scope_at_mut(start, depth))
                            .and_then(|scope| match scope.slots.get_mut(index as usize) {
                                Some(
                                    Value::Binding(_)
                                    | Value::ConstBinding(_)
                                    | Value::Uninitialized,
                                )
                                | None => None,
                                Some(slot) => {
                                    let value = if len > 0 {
                                        self.vm_stack.get_unchecked(len - 1).clone()
                                    } else {
                                        Value::Undefined
                                    };
                                    Some(std::mem::replace(slot, value))
                                }
                            })
                    };
                    if let Some(old) = old_value {
                        old.fast_drop();
                    } else {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let value = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                        self.write_local(top, depth, index, name, value)?;
                    }
                }
                Op::SetLocalPop { depth, index } => {
                    let value = pop_fast(&mut self.vm_stack);
                    let written = unsafe {
                        self.frames
                            .get_unchecked(top)
                            .scope
                            .as_ref()
                            .and_then(|start| (&*heap_ptr).scope_at_mut(start, depth))
                            .and_then(|scope| match scope.slots.get_mut(index as usize) {
                                Some(
                                    Value::Binding(_)
                                    | Value::ConstBinding(_)
                                    | Value::Uninitialized,
                                )
                                | None => None,
                                Some(slot) => Some(slot),
                            })
                    };
                    if let Some(slot) = written {
                        let old = std::mem::replace(slot, value);
                        old.fast_drop();
                    } else {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let name = self.local_layout_name(top, depth, index);
                        self.write_local_named(top, depth, index, &name, value)?;
                    }
                }
                Op::InitLocal { depth, index } => {
                    let value = pop_fast(&mut self.vm_stack);
                    let fast = unsafe {
                        self.frames
                            .get_unchecked(top)
                            .scope
                            .as_ref()
                            .and_then(|start| (&*heap_ptr).scope_at_mut(start, depth))
                    };
                    if let Some(scope) = fast {
                        match &scope.slots[index as usize] {
                            Value::Binding(cell) | Value::ConstBinding(cell) => {
                                let old = std::mem::replace(&mut *cell.borrow_mut(), value);
                                old.fast_drop();
                            }
                            _ => {
                                let old =
                                    std::mem::replace(&mut scope.slots[index as usize], value);
                                old.fast_drop();
                            }
                        }
                    } else {
                        let scope_ref = Self::scope_at(&self.frames[top], depth);
                        let mut scope = scope_ref.borrow_mut();
                        match &scope.slots[index as usize] {
                            Value::Binding(cell) | Value::ConstBinding(cell) => {
                                let old = std::mem::replace(&mut *cell.borrow_mut(), value);
                                old.fast_drop();
                            }
                            _ => {
                                let old =
                                    std::mem::replace(&mut scope.slots[index as usize], value);
                                old.fast_drop();
                            }
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
                    let fast = unsafe {
                        self.frames
                            .get_unchecked(top)
                            .scope
                            .as_ref()
                            .and_then(|start| (&*heap_ptr).scope_at_mut(start, depth))
                            .and_then(|scope| {
                                let Value::Number(slot) = scope.slots.get_mut(index as usize)?
                                else {
                                    return None;
                                };
                                let old = *slot;
                                let new = if increment { old + 1.0 } else { old - 1.0 };
                                *slot = new;
                                Some(Value::Number(if prefix { new } else { old }))
                            })
                    };
                    match fast {
                        Some(value) => unsafe { push_unchecked(&mut self.vm_stack, value) },
                        None => {
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            self.update_local_slow(top, depth, index, name, increment, prefix)?;
                            reserve_headroom(&mut self.vm_stack, unsafe {
                                (&*code).max_pushes as usize
                            });
                        }
                    }
                }
                Op::UpdateLocalPop {
                    depth,
                    index,
                    increment,
                } => {
                    let fast = unsafe {
                        self.frames
                            .get_unchecked(top)
                            .scope
                            .as_ref()
                            .and_then(|start| (&*heap_ptr).scope_at_mut(start, depth))
                            .is_some_and(|scope| {
                                if let Some(Value::Number(slot)) =
                                    scope.slots.get_mut(index as usize)
                                {
                                    *slot = if increment { *slot + 1.0 } else { *slot - 1.0 };
                                    true
                                } else {
                                    false
                                }
                            })
                    };
                    if !fast {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let name = self.local_layout_name(top, depth, index);
                        self.update_local_slow_named(top, depth, index, &name, increment, false)?;
                        pop_fast(&mut self.vm_stack).fast_drop();
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                    }
                }
                Op::GetGlobal(name) => {
                    let key: &str = unsafe { (&*code).names.get_unchecked(name as usize) };
                    let fast = {
                        let global = self.global.borrow();
                        if global.accessors.is_empty() || !global.accessors.contains_key(key) {
                            match global.values.get(key) {
                                Some(Value::Uninitialized) | None => None,
                                Some(value) => Some(value.resolve()),
                            }
                        } else {
                            None
                        }
                    };
                    match fast {
                        Some(value) => unsafe { push_unchecked(&mut self.vm_stack, value) },
                        None => {
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            let key = unsafe { (&*code).names[name as usize].clone() };
                            let value = self.get_global(&key)?;
                            reserve_headroom(&mut self.vm_stack, unsafe {
                                (&*code).max_pushes as usize
                            });
                            unsafe { push_unchecked(&mut self.vm_stack, value) };
                        }
                    }
                }
                Op::PushScope(block) => {
                    let layout = unsafe { (&*code).blocks.get_unchecked(block as usize).clone() };
                    let frame = unsafe { self.frames.get_unchecked_mut(top) };
                    let parent = frame.scope.clone();
                    let heap = unsafe { &*heap_ptr };
                    let slots = heap.alloc_scope_slots(layout.names.len(), true);
                    let scope = heap.alloc_scope_fast(Scope {
                        slots,
                        parent,
                        layout,
                        extra: Grown::default(),
                    });
                    frame.scope = Some(scope);
                    frame.scopes += 1;
                }
                Op::PushVarScope(block) => {
                    let layout = unsafe { (&*code).blocks.get_unchecked(block as usize).clone() };
                    let frame = unsafe { self.frames.get_unchecked_mut(top) };
                    let parent = frame.scope.clone();
                    let heap = unsafe { &*heap_ptr };
                    let slots = heap.alloc_scope_slots(layout.names.len(), false);
                    let scope = heap.alloc_scope_fast(Scope {
                        slots,
                        parent,
                        layout,
                        extra: Grown::default(),
                    });
                    frame.scope = Some(scope);
                    frame.scopes += 1;
                }
                Op::PopScope => {
                    let frame = unsafe { self.frames.get_unchecked_mut(top) };
                    if frame.scopes > 0 {
                        let depth = frame.scopes;
                        let popped = frame.scope.take();
                        let parent = popped.as_ref().and_then(|scope| {
                            unsafe { (&*heap_ptr).scope_at_ref(scope, 0) }
                                .map(|s| s.parent.clone())
                                .unwrap_or_else(|| scope.borrow().parent.clone())
                        });
                        if depth >= frame.escaped_scopes
                            && let Some(scope) = popped
                        {
                            unsafe { &*heap_ptr }.free_scope_fast(scope);
                        }
                        frame.scope = parent;
                        frame.scopes -= 1;
                        if frame.escaped_scopes > frame.scopes + 1 {
                            frame.escaped_scopes = frame.scopes + 1;
                        }
                    }
                }
                Op::CloneScope => {
                    let frame = unsafe { self.frames.get_unchecked_mut(top) };
                    let depth = frame.scopes;
                    let current = frame.scope.take().expect("a scope to copy");
                    let heap = unsafe { &*heap_ptr };
                    let cloned = if let Some(scope) = unsafe { heap.scope_at_ref(&current, 0) } {
                        Scope {
                            slots: heap.clone_scope_slots(&scope.slots),
                            parent: scope.parent.clone(),
                            layout: scope.layout.clone(),
                            extra: scope.extra.clone(),
                        }
                    } else {
                        let scope = current.borrow();
                        Scope {
                            slots: heap.clone_scope_slots(&scope.slots),
                            parent: scope.parent.clone(),
                            layout: scope.layout.clone(),
                            extra: scope.extra.clone(),
                        }
                    };
                    if depth > 0 && depth >= frame.escaped_scopes {
                        heap.free_scope_fast(current);
                    }
                    if frame.escaped_scopes > depth {
                        frame.escaped_scopes = depth;
                    }
                    frame.scope = Some(heap.alloc_scope_fast(cloned));
                }
                Op::This => {
                    let frame = unsafe { self.frames.get_unchecked(top) };
                    if matches!(frame.this, Value::Uninitialized) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        return Err(self
                            .reference_error("must call super constructor before accessing this"));
                    }
                    let this = frame.this.clone();
                    unsafe { push_unchecked(&mut self.vm_stack, this) };
                }
                Op::Not => {
                    let value = pop_fast(&mut self.vm_stack);
                    let result = !value.truthy();
                    value.fast_drop();
                    unsafe { push_unchecked(&mut self.vm_stack, Value::Bool(result)) };
                }
                Op::TypeOf => {
                    let value = pop_fast(&mut self.vm_stack);
                    let kind = self.type_of(&value);
                    value.fast_drop();
                    unsafe { push_unchecked(&mut self.vm_stack, Value::String(type_name(kind))) };
                }
                Op::ToIndexKey => {
                    let len = self.vm_stack.len();
                    if len >= 2
                        && matches!(
                            self.vm_stack[len - 2],
                            Value::Undefined | Value::Null | Value::Uninitialized
                        )
                    {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        return Err(self.type_error("cannot read properties of null or undefined"));
                    }
                    let keep = match self.vm_stack.last() {
                        Some(Value::String(_)) => true,
                        Some(Value::Number(number)) => {
                            *number >= 0.0 && number.fract() == 0.0 && *number < 4_294_967_295.0
                        }
                        _ => false,
                    };
                    if !keep {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let value = pop_fast(&mut self.vm_stack);
                        let key = self.coerce_key(value)?;
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                        unsafe { push_unchecked(&mut self.vm_stack, Value::String(key.into())) };
                    }
                }
                Op::Binary(operator) => {
                    let len = self.vm_stack.len();
                    if len >= 2 {
                        let left_ref = unsafe { self.vm_stack.get_unchecked(len - 2) };
                        let right_ref = unsafe { self.vm_stack.get_unchecked(len - 1) };
                        if let (Value::Number(left), Value::Number(right)) = (left_ref, right_ref)
                            && let Some(value) =
                                super::operations::number_binary(operator, *left, *right)
                        {
                            // safety: both operands are numbers with no drop glue
                            unsafe {
                                self.vm_stack.as_mut_ptr().add(len - 2).write(value);
                                self.vm_stack.set_len(len - 1);
                            }
                            continue;
                        }
                        if matches!(
                            operator,
                            super::ast::BinaryOperator::StrictEqual
                                | super::ast::BinaryOperator::StrictNotEqual
                        ) {
                            let equal = values_equal(left_ref, right_ref);
                            let right = pop_fast(&mut self.vm_stack);
                            let left = pop_fast(&mut self.vm_stack);
                            right.fast_drop();
                            left.fast_drop();
                            let result = equal
                                == matches!(operator, super::ast::BinaryOperator::StrictEqual);
                            unsafe { push_unchecked(&mut self.vm_stack, Value::Bool(result)) };
                            continue;
                        }
                    }
                    unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                    let right = pop_fast(&mut self.vm_stack);
                    let left = pop_fast(&mut self.vm_stack);
                    let value = self.binary(operator, left, right)?;
                    reserve_headroom(&mut self.vm_stack, unsafe { (&*code).max_pushes as usize });
                    unsafe { push_unchecked(&mut self.vm_stack, value) };
                }
                Op::BinaryRegs {
                    left,
                    right,
                    op: operator,
                } => {
                    let left_ref = unsafe { self.vm_stack.get_unchecked(reg_base + left as usize) };
                    if matches!(left_ref, Value::Uninitialized) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let name = unsafe { (&*code).layout.names[left as usize].clone() };
                        return Err(self.reference_error(format!(
                            "cannot access {name} before initialization"
                        )));
                    }
                    let right_ref =
                        unsafe { self.vm_stack.get_unchecked(reg_base + right as usize) };
                    if matches!(right_ref, Value::Uninitialized) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let name = unsafe { (&*code).layout.names[right as usize].clone() };
                        return Err(self.reference_error(format!(
                            "cannot access {name} before initialization"
                        )));
                    }
                    if let (Value::Number(l), Value::Number(r)) = (left_ref, right_ref)
                        && let Some(value) = super::operations::number_binary(operator, *l, *r)
                    {
                        unsafe { push_unchecked(&mut self.vm_stack, value) };
                        continue;
                    }
                    if matches!(
                        operator,
                        super::ast::BinaryOperator::StrictEqual
                            | super::ast::BinaryOperator::StrictNotEqual
                    ) {
                        let equal = values_equal(left_ref, right_ref);
                        let result =
                            equal == matches!(operator, super::ast::BinaryOperator::StrictEqual);
                        unsafe { push_unchecked(&mut self.vm_stack, Value::Bool(result)) };
                        continue;
                    }
                    let l = left_ref.clone();
                    let r = right_ref.clone();
                    unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                    let value = self.binary(operator, l, r)?;
                    reserve_headroom(&mut self.vm_stack, unsafe { (&*code).max_pushes as usize });
                    unsafe { push_unchecked(&mut self.vm_stack, value) };
                }
                Op::Jump(target) => {
                    let target = target as usize;
                    if target <= op_pc {
                        self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                        if self.steps >= safepoint_at {
                            safepoint_at = self.hit_safepoint(sampling, target)?;
                        }
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                        #[cfg(all(
                            target_arch = "x86_64",
                            any(target_os = "windows", target_os = "linux")
                        ))]
                        {
                            try_jit = true;
                        }
                    }
                    pc = target;
                    block_start = pc;
                }
                Op::JumpIfFalse(target) | Op::JumpIfTrue(target) => {
                    let value = pop_fast(&mut self.vm_stack);
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
                            reserve_headroom(&mut self.vm_stack, unsafe {
                                (&*code).max_pushes as usize
                            });
                        }
                        pc = target;
                        block_start = pc;
                    }
                }
                Op::JumpIfRegFalse { reg, target } | Op::JumpIfRegTrue { reg, target } => {
                    let value = unsafe { self.vm_stack.get_unchecked(reg_base + reg as usize) };
                    if matches!(value, Value::Uninitialized) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let name = unsafe { (&*code).layout.names[reg as usize].clone() };
                        return Err(self.reference_error(format!(
                            "cannot access {name} before initialization"
                        )));
                    }
                    let test = match value {
                        Value::Bool(test) => *test,
                        other => other.truthy(),
                    };
                    if test == matches!(op, Op::JumpIfRegTrue { .. }) {
                        let target = target as usize;
                        if target <= op_pc {
                            self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                            if self.steps >= safepoint_at {
                                safepoint_at = self.hit_safepoint(sampling, target)?;
                            }
                            reserve_headroom(&mut self.vm_stack, unsafe {
                                (&*code).max_pushes as usize
                            });
                        }
                        pc = target;
                        block_start = pc;
                    }
                }
                Op::JumpIfNotCompare {
                    op: operator,
                    target,
                } => {
                    let len = self.vm_stack.len();
                    let passed = if len >= 2 {
                        let left_ref = unsafe { self.vm_stack.get_unchecked(len - 2) };
                        let right_ref = unsafe { self.vm_stack.get_unchecked(len - 1) };
                        if let Some(passed) = fast_compare(operator, left_ref, right_ref) {
                            let right = pop_fast(&mut self.vm_stack);
                            let left = pop_fast(&mut self.vm_stack);
                            right.fast_drop();
                            left.fast_drop();
                            passed
                        } else {
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            let right = pop_fast(&mut self.vm_stack);
                            let left = pop_fast(&mut self.vm_stack);
                            let result = self.binary(operator, left, right)?;
                            let truthy = result.truthy();
                            result.fast_drop();
                            reserve_headroom(&mut self.vm_stack, unsafe {
                                (&*code).max_pushes as usize
                            });
                            truthy
                        }
                    } else {
                        false
                    };
                    if !passed {
                        let target = target as usize;
                        if target <= op_pc {
                            self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                            if self.steps >= safepoint_at {
                                safepoint_at = self.hit_safepoint(sampling, target)?;
                            }
                            reserve_headroom(&mut self.vm_stack, unsafe {
                                (&*code).max_pushes as usize
                            });
                        }
                        pc = target;
                        block_start = pc;
                    }
                }
                Op::JumpIfNotCompareRegs {
                    left,
                    right,
                    op: operator,
                    target,
                } => {
                    let left_ref = unsafe { self.vm_stack.get_unchecked(reg_base + left as usize) };
                    if matches!(left_ref, Value::Uninitialized) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let name = unsafe { (&*code).layout.names[left as usize].clone() };
                        return Err(self.reference_error(format!(
                            "cannot access {name} before initialization"
                        )));
                    }
                    let right_ref =
                        unsafe { self.vm_stack.get_unchecked(reg_base + right as usize) };
                    if matches!(right_ref, Value::Uninitialized) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let name = unsafe { (&*code).layout.names[right as usize].clone() };
                        return Err(self.reference_error(format!(
                            "cannot access {name} before initialization"
                        )));
                    }
                    let passed = match fast_compare(operator, left_ref, right_ref) {
                        Some(passed) => passed,
                        None => {
                            let l = left_ref.clone();
                            let r = right_ref.clone();
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            let result = self.binary(operator, l, r)?;
                            let truthy = result.truthy();
                            result.fast_drop();
                            reserve_headroom(&mut self.vm_stack, unsafe {
                                (&*code).max_pushes as usize
                            });
                            truthy
                        }
                    };
                    if !passed {
                        let target = target as usize;
                        if target <= op_pc {
                            self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                            if self.steps >= safepoint_at {
                                safepoint_at = self.hit_safepoint(sampling, target)?;
                            }
                            reserve_headroom(&mut self.vm_stack, unsafe {
                                (&*code).max_pushes as usize
                            });
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
                            reserve_headroom(&mut self.vm_stack, unsafe {
                                (&*code).max_pushes as usize
                            });
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
                        let target = target as usize;
                        if target <= op_pc {
                            reserve_headroom(&mut self.vm_stack, unsafe {
                                (&*code).max_pushes as usize
                            });
                        }
                        pc = target;
                        block_start = pc;
                    }
                }
                Op::JumpIfNotUndefined(target) => {
                    if !matches!(self.vm_stack.last(), Some(Value::Undefined) | None) {
                        let target = target as usize;
                        if target <= op_pc {
                            reserve_headroom(&mut self.vm_stack, unsafe {
                                (&*code).max_pushes as usize
                            });
                        }
                        pc = target;
                        block_start = pc;
                    }
                }
                Op::NewObject => {
                    let object = unsafe { &*heap_ptr }.alloc_object_fast(JsObject::with_prototype(
                        Some(self.intrinsics.object_prototype.clone()),
                        ObjectClass::Ordinary,
                    ));
                    unsafe { push_unchecked(&mut self.vm_stack, Value::Object(object)) };
                }
                Op::MakeObject(template) => {
                    let (keys, cell) =
                        unsafe { (&*code).object_shapes.get_unchecked(template as usize) };
                    let shape = cell
                        .get_or_init(|| {
                            let names: Vec<&str> = keys.iter().map(|name| &**name).collect();
                            super::object::Shape::from_keys(&names)
                        })
                        .clone();
                    let count = shape.len();
                    let start = self.vm_stack.len() - count;
                    let mut slots = Vec::with_capacity(count);
                    // safety: moving count live Values off the tail of vm_stack into slots
                    unsafe {
                        let src = self.vm_stack.as_ptr().add(start);
                        for i in 0..count {
                            slots.push(Slot::Data(std::ptr::read(src.add(i))));
                        }
                        self.vm_stack.set_len(start);
                    }
                    let object = unsafe { &*heap_ptr }.alloc_object_fast(JsObject::from_shape(
                        shape,
                        slots,
                        Some(self.intrinsics.object_prototype.clone()),
                    ));
                    unsafe { push_unchecked(&mut self.vm_stack, Value::Object(object)) };
                }
                Op::NewArray => {
                    let array = unsafe { &*heap_ptr }.alloc_array_fast(ArrayData::default());
                    unsafe { push_unchecked(&mut self.vm_stack, Value::Array(array)) };
                }
                Op::MakeArray(count) => {
                    let count = count as usize;
                    let values = if count == 0 {
                        Vec::new()
                    } else {
                        let start = self.vm_stack.len() - count;
                        self.vm_stack.split_off(start)
                    };
                    let array = unsafe { &*heap_ptr }.alloc_array_fast(ArrayData::from(values));
                    unsafe { push_unchecked(&mut self.vm_stack, Value::Array(array)) };
                }
                Op::ArrayPush => {
                    let value = pop_fast(&mut self.vm_stack);
                    if let Some(Value::Array(array)) = self.vm_stack.last() {
                        if let Some(data) = unsafe { (&*heap_ptr).array_mut(array) } {
                            data.values.push(value);
                        } else {
                            array.borrow_mut().push(value);
                        }
                    } else {
                        value.fast_drop();
                    }
                }
                Op::DefineField(name) => {
                    let value = pop_fast(&mut self.vm_stack);
                    if let Some(Value::Object(object)) = self.vm_stack.last() {
                        let key: &str = unsafe { (&*code).names.get_unchecked(name as usize) };
                        if let Some(target) = unsafe { (&*heap_ptr).object_mut(object) } {
                            target.define_key(key, Property::data(value));
                        } else {
                            object.borrow_mut().define_key(key, Property::data(value));
                        }
                    } else {
                        value.fast_drop();
                    }
                }
                Op::Closure(index) => {
                    let child = unsafe { (&*code).children.get_unchecked(index as usize).clone() };
                    let frame = unsafe { self.frames.get_unchecked_mut(top) };
                    let scope = if child.captures_nothing {
                        None
                    } else {
                        frame.mark_scopes_escaped();
                        frame.scope.clone()
                    };
                    let home = if child.kind == FunctionKind::Arrow {
                        frame.closure.as_ref().and_then(|closure| closure.home())
                    } else {
                        None
                    };
                    let lexical_this = child.uses_lexical_this.then(|| frame.this.clone());
                    let closure = Rc::new(Closure::new(child, scope));
                    if let Some(home) = home {
                        closure.set_home_object(home);
                    }
                    if let Some(this) = lexical_this {
                        closure.set_lexical_this(this);
                    }
                    let function = self.function_object(closure);
                    unsafe { push_unchecked(&mut self.vm_stack, function) };
                }
                Op::GetProp { name, ic } => {
                    use super::object::IcGetHit;
                    let cache = unsafe { (&*code).ics.get_unchecked(ic as usize) };
                    let hit = self
                        .vm_stack
                        .last()
                        .and_then(|recv| self.ic_get_value(unsafe { &*heap_ptr }, recv, cache));
                    match hit {
                        Some(IcGetHit::Value(value)) => {
                            let len = self.vm_stack.len();
                            let old = std::mem::replace(
                                unsafe { self.vm_stack.get_unchecked_mut(len - 1) },
                                value,
                            );
                            old.fast_drop();
                        }
                        Some(IcGetHit::Missing) => {
                            let len = self.vm_stack.len();
                            let old = std::mem::replace(
                                unsafe { self.vm_stack.get_unchecked_mut(len - 1) },
                                Value::Undefined,
                            );
                            old.fast_drop();
                        }
                        Some(IcGetHit::Getter(getter)) => {
                            // push getter before safepoint so GC traces it on vm_stack
                            unsafe { push_unchecked(&mut self.vm_stack, getter) };
                            self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                            if self.steps >= safepoint_at {
                                safepoint_at = self.hit_safepoint(sampling, op_pc)?;
                            }
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            if self.call_ic_getter(unsafe { &*heap_ptr })? {
                                reload_frame!();
                            } else {
                                reserve_headroom(&mut self.vm_stack, unsafe {
                                    (&*code).max_pushes as usize
                                });
                            }
                        }
                        None => {
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            if self.get_prop_slow(name, ic)? {
                                reload_frame!();
                            } else {
                                reserve_headroom(&mut self.vm_stack, unsafe {
                                    (&*code).max_pushes as usize
                                });
                            }
                        }
                    }
                }
                Op::GetRegProp { reg, name, ic } => {
                    use super::object::IcGetHit;
                    let receiver = unsafe { self.vm_stack.get_unchecked(reg_base + reg as usize) };
                    if matches!(receiver, Value::Uninitialized) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let name = unsafe { (&*code).layout.names[reg as usize].clone() };
                        return Err(self.reference_error(format!(
                            "cannot access {name} before initialization"
                        )));
                    }
                    let cache = unsafe { (&*code).ics.get_unchecked(ic as usize) };
                    match self.ic_get_value(unsafe { &*heap_ptr }, receiver, cache) {
                        Some(IcGetHit::Value(value)) => unsafe {
                            push_unchecked(&mut self.vm_stack, value);
                        },
                        Some(IcGetHit::Missing) => unsafe {
                            push_unchecked(&mut self.vm_stack, Value::Undefined);
                        },
                        Some(IcGetHit::Getter(getter)) => {
                            let recv = receiver.clone();
                            unsafe {
                                push_unchecked(&mut self.vm_stack, recv);
                                push_unchecked(&mut self.vm_stack, getter);
                            }
                            self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                            if self.steps >= safepoint_at {
                                safepoint_at = self.hit_safepoint(sampling, op_pc)?;
                            }
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            if self.call_ic_getter(unsafe { &*heap_ptr })? {
                                reload_frame!();
                            } else {
                                reserve_headroom(&mut self.vm_stack, unsafe {
                                    (&*code).max_pushes as usize
                                });
                            }
                        }
                        None => {
                            let recv = receiver.clone();
                            unsafe { push_unchecked(&mut self.vm_stack, recv) };
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            if self.get_prop_slow(name, ic)? {
                                reload_frame!();
                            } else {
                                reserve_headroom(&mut self.vm_stack, unsafe {
                                    (&*code).max_pushes as usize
                                });
                            }
                        }
                    }
                }
                Op::GetLocalProp {
                    depth,
                    index,
                    name,
                    ic,
                } => {
                    use super::object::IcGetHit;
                    let cache = unsafe { (&*code).ics.get_unchecked(ic as usize) };
                    let probe = unsafe {
                        let heap = &*heap_ptr;
                        self.frames
                            .get_unchecked(top)
                            .scope
                            .as_ref()
                            .and_then(|start| heap.scope_at_ref(start, depth))
                            .and_then(|scope| match scope.slots.get(index as usize) {
                                Some(Value::Binding(cell) | Value::ConstBinding(cell)) => {
                                    let inner = cell.borrow();
                                    if matches!(*inner, Value::Uninitialized) {
                                        return None;
                                    }
                                    match self.ic_get_value(heap, &inner, cache) {
                                        Some(IcGetHit::Value(v)) => Some(Ok(IcGetHit::Value(v))),
                                        Some(IcGetHit::Missing) => Some(Ok(IcGetHit::Missing)),
                                        Some(IcGetHit::Getter(g)) => {
                                            Some(Err((inner.clone(), Some(g))))
                                        }
                                        None => Some(Err((inner.clone(), None))),
                                    }
                                }
                                Some(Value::Uninitialized) | None => None,
                                Some(receiver) => match self.ic_get_value(heap, receiver, cache) {
                                    Some(IcGetHit::Value(v)) => Some(Ok(IcGetHit::Value(v))),
                                    Some(IcGetHit::Missing) => Some(Ok(IcGetHit::Missing)),
                                    Some(IcGetHit::Getter(g)) => {
                                        Some(Err((receiver.clone(), Some(g))))
                                    }
                                    None => Some(Err((receiver.clone(), None))),
                                },
                            })
                    };
                    match probe {
                        Some(Ok(IcGetHit::Value(value))) => unsafe {
                            push_unchecked(&mut self.vm_stack, value);
                        },
                        Some(Ok(IcGetHit::Missing)) => unsafe {
                            push_unchecked(&mut self.vm_stack, Value::Undefined);
                        },
                        Some(Err((recv, Some(getter)))) => {
                            unsafe {
                                push_unchecked(&mut self.vm_stack, recv);
                                push_unchecked(&mut self.vm_stack, getter);
                            }
                            self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                            if self.steps >= safepoint_at {
                                safepoint_at = self.hit_safepoint(sampling, op_pc)?;
                            }
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            if self.call_ic_getter(unsafe { &*heap_ptr })? {
                                reload_frame!();
                            } else {
                                reserve_headroom(&mut self.vm_stack, unsafe {
                                    (&*code).max_pushes as usize
                                });
                            }
                        }
                        Some(Err((recv, None))) => {
                            unsafe { push_unchecked(&mut self.vm_stack, recv) };
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            if self.get_prop_slow(name, ic)? {
                                reload_frame!();
                            } else {
                                reserve_headroom(&mut self.vm_stack, unsafe {
                                    (&*code).max_pushes as usize
                                });
                            }
                        }
                        Some(Ok(IcGetHit::Getter(_))) => unreachable!(),
                        None => {
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            let key = self.local_layout_name(top, depth, index);
                            let recv = self.read_local_named(top, depth, index, &key)?;
                            self.vm_stack.push(recv);
                            if self.get_prop_slow(name, ic)? {
                                reload_frame!();
                            } else {
                                reserve_headroom(&mut self.vm_stack, unsafe {
                                    (&*code).max_pushes as usize
                                });
                            }
                        }
                    }
                }
                Op::GetRegMethod { reg, name, ic } => {
                    use super::object::IcGetHit;
                    let receiver = unsafe { self.vm_stack.get_unchecked(reg_base + reg as usize) };
                    if matches!(receiver, Value::Uninitialized) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let name = unsafe { (&*code).layout.names[reg as usize].clone() };
                        return Err(self.reference_error(format!(
                            "cannot access {name} before initialization"
                        )));
                    }
                    let recv = receiver.clone();
                    let cache = unsafe { (&*code).ics.get_unchecked(ic as usize) };
                    match self.ic_get_value(unsafe { &*heap_ptr }, &recv, cache) {
                        Some(IcGetHit::Value(value)) => unsafe {
                            push_unchecked(&mut self.vm_stack, recv);
                            push_unchecked(&mut self.vm_stack, value);
                        },
                        Some(IcGetHit::Missing) => unsafe {
                            push_unchecked(&mut self.vm_stack, recv);
                            push_unchecked(&mut self.vm_stack, Value::Undefined);
                        },
                        Some(IcGetHit::Getter(getter)) => {
                            let recv2 = recv.clone();
                            unsafe {
                                push_unchecked(&mut self.vm_stack, recv);
                                push_unchecked(&mut self.vm_stack, recv2);
                                push_unchecked(&mut self.vm_stack, getter);
                            }
                            self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                            if self.steps >= safepoint_at {
                                safepoint_at = self.hit_safepoint(sampling, op_pc)?;
                            }
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            if self.call_ic_getter(unsafe { &*heap_ptr })? {
                                reload_frame!();
                            } else {
                                reserve_headroom(&mut self.vm_stack, unsafe {
                                    (&*code).max_pushes as usize
                                });
                            }
                        }
                        None => {
                            let recv2 = recv.clone();
                            unsafe {
                                push_unchecked(&mut self.vm_stack, recv);
                                push_unchecked(&mut self.vm_stack, recv2);
                            }
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            if self.get_prop_slow(name, ic)? {
                                reload_frame!();
                            } else {
                                reserve_headroom(&mut self.vm_stack, unsafe {
                                    (&*code).max_pushes as usize
                                });
                            }
                        }
                    }
                }
                Op::GetLocalMethod {
                    depth,
                    index,
                    name,
                    ic,
                } => {
                    use super::object::IcGetHit;
                    let cache = unsafe { (&*code).ics.get_unchecked(ic as usize) };
                    let recv = unsafe {
                        self.frames
                            .get_unchecked(top)
                            .scope
                            .as_ref()
                            .and_then(|start| (&*heap_ptr).scope_at_ref(start, depth))
                            .and_then(|scope| match scope.slots.get(index as usize) {
                                Some(Value::Binding(cell) | Value::ConstBinding(cell)) => {
                                    let inner = cell.borrow();
                                    (!matches!(*inner, Value::Uninitialized)).then(|| inner.clone())
                                }
                                Some(Value::Uninitialized) | None => None,
                                Some(value) => Some(value.clone()),
                            })
                    };
                    let recv = match recv {
                        Some(value) => value,
                        None => {
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            let key = self.local_layout_name(top, depth, index);
                            self.read_local_named(top, depth, index, &key)?
                        }
                    };
                    match self.ic_get_value(unsafe { &*heap_ptr }, &recv, cache) {
                        Some(IcGetHit::Value(value)) => unsafe {
                            push_unchecked(&mut self.vm_stack, recv);
                            push_unchecked(&mut self.vm_stack, value);
                        },
                        Some(IcGetHit::Missing) => unsafe {
                            push_unchecked(&mut self.vm_stack, recv);
                            push_unchecked(&mut self.vm_stack, Value::Undefined);
                        },
                        Some(IcGetHit::Getter(getter)) => {
                            let recv2 = recv.clone();
                            unsafe {
                                push_unchecked(&mut self.vm_stack, recv);
                                push_unchecked(&mut self.vm_stack, recv2);
                                push_unchecked(&mut self.vm_stack, getter);
                            }
                            self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                            if self.steps >= safepoint_at {
                                safepoint_at = self.hit_safepoint(sampling, op_pc)?;
                            }
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            if self.call_ic_getter(unsafe { &*heap_ptr })? {
                                reload_frame!();
                            } else {
                                reserve_headroom(&mut self.vm_stack, unsafe {
                                    (&*code).max_pushes as usize
                                });
                            }
                        }
                        None => {
                            let recv2 = recv.clone();
                            unsafe {
                                push_unchecked(&mut self.vm_stack, recv);
                                push_unchecked(&mut self.vm_stack, recv2);
                            }
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            if self.get_prop_slow(name, ic)? {
                                reload_frame!();
                            } else {
                                reserve_headroom(&mut self.vm_stack, unsafe {
                                    (&*code).max_pushes as usize
                                });
                            }
                        }
                    }
                }
                Op::SetProp { name, ic } => {
                    let len = self.vm_stack.len();
                    let cache = unsafe { (&*code).ics.get_unchecked(ic as usize) };
                    let hit = len >= 2
                        && match unsafe { self.vm_stack.get_unchecked(len - 2) } {
                            Value::Object(handle) => {
                                let value = unsafe { self.vm_stack.get_unchecked(len - 1).clone() };
                                unsafe { (&*heap_ptr).object_mut(handle) }.is_some_and(|target| {
                                    target
                                        .ic_set_fast(handle, unsafe { &*heap_ptr }, cache, value)
                                        .is_ok()
                                })
                            }
                            _ => false,
                        };
                    if hit {
                        let value = pop_fast(&mut self.vm_stack);
                        let old = std::mem::replace(
                            unsafe { self.vm_stack.get_unchecked_mut(len - 2) },
                            value,
                        );
                        old.fast_drop();
                    } else {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        self.set_prop_slow(name, ic)?;
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                    }
                }
                Op::SetPropPop { name, ic } => {
                    let value = pop_fast(&mut self.vm_stack);
                    let object = pop_fast(&mut self.vm_stack);
                    let cache = unsafe { (&*code).ics.get_unchecked(ic as usize) };
                    let missed = match &object {
                        Value::Object(handle) => {
                            let heap = unsafe { &*heap_ptr };
                            match unsafe { heap.object_mut(handle) } {
                                Some(target) => target.ic_set_fast(handle, heap, cache, value),
                                None => Err(value),
                            }
                        }
                        _ => Err(value),
                    };
                    if let Err(value) = missed {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        self.set_prop_slow_with(object, value, name, ic, false)?;
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                    } else {
                        object.fast_drop();
                    }
                }
                Op::GetIndex => {
                    let len = self.vm_stack.len();
                    if len >= 2
                        && let Some(value) = Self::try_fast_get_index(
                            unsafe { &*heap_ptr },
                            unsafe { self.vm_stack.get_unchecked(len - 2) },
                            unsafe { self.vm_stack.get_unchecked(len - 1) },
                        )
                    {
                        let key = pop_fast(&mut self.vm_stack);
                        key.fast_drop();
                        let old = std::mem::replace(
                            unsafe { self.vm_stack.get_unchecked_mut(len - 2) },
                            value,
                        );
                        old.fast_drop();
                    } else {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let key = pop_fast(&mut self.vm_stack);
                        let object = pop_fast(&mut self.vm_stack);
                        match self.get_index_tail(&object, key)? {
                            super::operations::GetOutcome::Value(value) => {
                                reserve_headroom(&mut self.vm_stack, unsafe {
                                    (&*code).max_pushes as usize
                                });
                                unsafe { push_unchecked(&mut self.vm_stack, value) };
                            }
                            super::operations::GetOutcome::Call {
                                callee,
                                this,
                                arguments,
                            } => {
                                self.call_in_loop(callee, this, arguments)?;
                                reload_frame!();
                            }
                        }
                    }
                }
                Op::GetRegIndex { array, index } => {
                    let arr_ref = unsafe { self.vm_stack.get_unchecked(reg_base + array as usize) };
                    if matches!(arr_ref, Value::Uninitialized) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let name = unsafe { (&*code).layout.names[array as usize].clone() };
                        return Err(self.reference_error(format!(
                            "cannot access {name} before initialization"
                        )));
                    }
                    let idx_ref = unsafe { self.vm_stack.get_unchecked(reg_base + index as usize) };
                    if matches!(idx_ref, Value::Uninitialized) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let name = unsafe { (&*code).layout.names[index as usize].clone() };
                        return Err(self.reference_error(format!(
                            "cannot access {name} before initialization"
                        )));
                    }
                    if let Some(value) =
                        Self::try_fast_get_index(unsafe { &*heap_ptr }, arr_ref, idx_ref)
                    {
                        unsafe { push_unchecked(&mut self.vm_stack, value) };
                    } else {
                        let object = arr_ref.clone();
                        let key = idx_ref.clone();
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        match self.get_index_tail(&object, key)? {
                            super::operations::GetOutcome::Value(value) => {
                                reserve_headroom(&mut self.vm_stack, unsafe {
                                    (&*code).max_pushes as usize
                                });
                                unsafe { push_unchecked(&mut self.vm_stack, value) };
                            }
                            super::operations::GetOutcome::Call {
                                callee,
                                this,
                                arguments,
                            } => {
                                self.call_in_loop(callee, this, arguments)?;
                                reload_frame!();
                            }
                        }
                    }
                }
                Op::SetIndex => {
                    let len = self.vm_stack.len();
                    let hit = len >= 3 && {
                        let value = unsafe { self.vm_stack.get_unchecked(len - 1).clone() };
                        Self::try_fast_set_index(
                            unsafe { &*heap_ptr },
                            unsafe { self.vm_stack.get_unchecked(len - 3) },
                            unsafe { self.vm_stack.get_unchecked(len - 2) },
                            value,
                        )
                        .is_ok()
                    };
                    if hit {
                        let value = pop_fast(&mut self.vm_stack);
                        let key = pop_fast(&mut self.vm_stack);
                        key.fast_drop();
                        let old = std::mem::replace(
                            unsafe { self.vm_stack.get_unchecked_mut(len - 3) },
                            value,
                        );
                        old.fast_drop();
                    } else {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let value = pop_fast(&mut self.vm_stack);
                        let key = pop_fast(&mut self.vm_stack);
                        let object = pop_fast(&mut self.vm_stack);
                        let strict = unsafe { (&*code).strict };
                        self.set_index(&object, key, value.clone(), strict)?;
                        object.fast_drop();
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                        unsafe { push_unchecked(&mut self.vm_stack, value) };
                    }
                }
                Op::SetIndexPop => {
                    let value = pop_fast(&mut self.vm_stack);
                    let key = pop_fast(&mut self.vm_stack);
                    let object = pop_fast(&mut self.vm_stack);
                    match Self::try_fast_set_index(unsafe { &*heap_ptr }, &object, &key, value) {
                        Ok(()) => {
                            key.fast_drop();
                            object.fast_drop();
                        }
                        Err(value) => {
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            let strict = unsafe { (&*code).strict };
                            self.set_index(&object, key, value, strict)?;
                            object.fast_drop();
                            reserve_headroom(&mut self.vm_stack, unsafe {
                                (&*code).max_pushes as usize
                            });
                        }
                    }
                }
                Op::Call(count) | Op::CallMethod(count) => {
                    self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                    if self.steps >= safepoint_at {
                        safepoint_at = self.hit_safepoint(sampling, op_pc)?;
                    }
                    unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                    let method = matches!(op, Op::CallMethod(_));
                    let heap = unsafe { &*heap_ptr };
                    if self.call_plain(heap, count as usize, method) {
                        reload_frame!();
                    } else {
                        match self.call_fast_native(heap, count as usize, method)? {
                            FastNativeCall::Done => {
                                reserve_headroom(&mut self.vm_stack, unsafe {
                                    (&*code).max_pushes as usize
                                });
                            }
                            FastNativeCall::FramePushed => {
                                reload_frame!();
                            }
                            FastNativeCall::FallbackMethod(new_count) => {
                                let arguments = self.pop_arguments(new_count);
                                let callee = pop_fast(&mut self.vm_stack);
                                let this = pop_fast(&mut self.vm_stack);
                                self.call_in_loop(callee, this, arguments)?;
                                reload_frame!();
                            }
                            FastNativeCall::NotHandled => {
                                let arguments = self.pop_arguments(count as usize);
                                let callee = pop_fast(&mut self.vm_stack);
                                let this = if method {
                                    pop_fast(&mut self.vm_stack)
                                } else {
                                    Value::Undefined
                                };
                                self.call_in_loop(callee, this, arguments)?;
                                reload_frame!();
                            }
                        }
                    }
                }
                Op::GlobalThis => unsafe {
                    push_unchecked(&mut self.vm_stack, Value::Window);
                },
                Op::RequireCoercible => {
                    if matches!(
                        self.vm_stack.last(),
                        Some(Value::Undefined | Value::Null) | None
                    ) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        return Err(self.type_error("cannot destructure null or undefined"));
                    }
                }
                Op::Uninitialize { index, count } => {
                    if let Some(scope_ref) =
                        unsafe { self.frames.get_unchecked(top) }.scope.as_ref()
                    {
                        if let Some(scope) = unsafe { (&*heap_ptr).scope_at_mut(scope_ref, 0) } {
                            let end = (index as usize + count as usize).min(scope.slots.len());
                            for slot in &mut scope.slots[index as usize..end] {
                                let old = std::mem::replace(slot, Value::Uninitialized);
                                old.fast_drop();
                            }
                        } else {
                            let mut scope = scope_ref.borrow_mut();
                            let end = (index as usize + count as usize).min(scope.slots.len());
                            for slot in &mut scope.slots[index as usize..end] {
                                let old = std::mem::replace(slot, Value::Uninitialized);
                                old.fast_drop();
                            }
                        }
                    }
                }
                Op::Negate => {
                    if let Some(Value::Number(number)) = self.vm_stack.last_mut() {
                        *number = -*number;
                    } else {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let value = pop_fast(&mut self.vm_stack);
                        let result = self.negate_numeric(value)?;
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                        unsafe { push_unchecked(&mut self.vm_stack, result) };
                    }
                }
                Op::Plus => {
                    if !matches!(self.vm_stack.last(), Some(Value::Number(_))) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let value = pop_fast(&mut self.vm_stack);
                        let number = self.coerce_number(value)?;
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                        unsafe { push_unchecked(&mut self.vm_stack, Value::Number(number)) };
                    }
                }
                Op::Increment | Op::Decrement => {
                    let delta = if matches!(op, Op::Increment) {
                        1.0
                    } else {
                        -1.0
                    };
                    if let Some(Value::Number(number)) = self.vm_stack.last_mut() {
                        *number += delta;
                    } else {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let value = pop_fast(&mut self.vm_stack);
                        let delta_i = if matches!(op, Op::Increment) { 1 } else { -1 };
                        let result = match self.coerce_numeric(value)? {
                            Value::BigInt(value) => Value::BigInt(Rc::new(
                                value.add(&super::bigint::BigInt::from_i64(delta_i)),
                            )),
                            numeric => Value::Number(self.coerce_number(numeric)? + delta),
                        };
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                        unsafe { push_unchecked(&mut self.vm_stack, result) };
                    }
                }
                Op::ToPropertyKey => {
                    if !matches!(self.vm_stack.last(), Some(Value::String(_))) {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let value = pop_fast(&mut self.vm_stack);
                        let key = self.coerce_key(value)?;
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                        unsafe { push_unchecked(&mut self.vm_stack, Value::String(key.into())) };
                    }
                }
                Op::TemplateConcat(1) if matches!(self.vm_stack.last(), Some(Value::String(_))) => {
                }
                Op::ChainStart => {
                    let offset =
                        self.vm_stack.len() - unsafe { self.frames.get_unchecked(top) }.stack_base;
                    self.vm_chains.push(offset);
                }
                Op::ChainEnd => {
                    let base = unsafe { self.frames.get_unchecked(top) }.chain_base as usize;
                    if self.vm_chains.len() > base {
                        self.vm_chains.pop();
                    }
                }
                Op::JumpIfNullishChain(target) => {
                    if matches!(
                        self.vm_stack.last(),
                        Some(Value::Undefined | Value::Null) | None
                    ) {
                        let frame = unsafe { self.frames.get_unchecked(top) };
                        let stack_base = frame.stack_base;
                        let chain_base = frame.chain_base as usize;
                        let mark = if self.vm_chains.len() > chain_base {
                            stack_base + self.vm_chains.pop().expect("checked above")
                        } else {
                            self.vm_stack.len()
                        };
                        truncate_stack(&mut self.vm_stack, mark);
                        unsafe { push_unchecked(&mut self.vm_stack, Value::Undefined) };
                        pc = target as usize;
                        block_start = pc;
                    }
                }
                Op::PushHandler { catch, finally } => {
                    let frame = unsafe { self.frames.get_unchecked(top) };
                    let stack_offset = self.vm_stack.len() - frame.stack_base;
                    let chains = self.vm_chains.len() - frame.chain_base as usize;
                    let iters = self.vm_iters.len() - frame.iter_base as usize;
                    self.vm_handlers.push(Handler {
                        catch,
                        finally,
                        stack: stack_offset,
                        scopes: frame.scopes,
                        chains,
                        iters,
                    });
                }
                Op::PopHandler => {
                    let base = unsafe { self.frames.get_unchecked(top) }.handler_base as usize;
                    if self.vm_handlers.len() > base {
                        self.vm_handlers.pop();
                    }
                }
                Op::CompletionNormal => {
                    self.vm_pending.push(Pending::Normal);
                }
                Op::EndFinally => {
                    let pending_base =
                        unsafe { self.frames.get_unchecked(top) }.pending_base as usize;
                    let pending = if self.vm_pending.len() > pending_base {
                        self.vm_pending.pop().expect("checked above")
                    } else {
                        Pending::Normal
                    };
                    match pending {
                        Pending::Normal => {}
                        Pending::Throw(value) => {
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            return Err(self.throw_value(value));
                        }
                        Pending::Return(value) => {
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            if let Some(result) = self.do_return(frame_floor, value)? {
                                return Ok(result);
                            }
                            reload_frame!();
                        }
                        Pending::Jump {
                            target,
                            handlers,
                            scopes,
                        } => {
                            self.do_exit(target, handlers, scopes);
                            pc = unsafe { self.frames.get_unchecked(top) }.pc;
                            block_start = pc;
                        }
                    }
                }
                Op::Exit {
                    target,
                    handlers,
                    scopes,
                } => {
                    self.do_exit(target, handlers, scopes);
                    pc = unsafe { self.frames.get_unchecked(top) }.pc;
                    if pc <= op_pc {
                        self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                        if self.steps >= safepoint_at {
                            safepoint_at = self.hit_safepoint(sampling, pc)?;
                        }
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                    }
                    block_start = pc;
                }
                Op::ArgumentsIterator => {
                    let len = self.vm_stack.len();
                    let slot = unsafe { self.vm_stack.get_unchecked_mut(len - 1) };
                    let Value::Array(array) = slot.clone() else {
                        unreachable!("arguments arrive as an array");
                    };
                    let iter_slot = self.vm_iters.len() as u32;
                    self.vm_iters.push((array, 0));
                    *slot = Value::ArrayIter(iter_slot);
                }
                Op::GetIterator(callsite) => {
                    let fast = if let Some(Value::Array(array)) = self.vm_stack.last() {
                        let heap = unsafe { &*heap_ptr };
                        let plain_array =
                            unsafe { heap.array_ref(array) }.is_some_and(|a| a.extra.is_none());
                        let proto_ok = plain_array
                            && unsafe { heap.object_ref(&self.intrinsics.array_prototype) }
                                .is_some_and(|p| {
                                    p.array_iterator_intact(&self.intrinsics.array_values)
                                });
                        proto_ok.then(|| array.clone())
                    } else {
                        None
                    };
                    if let Some(array) = fast {
                        let len = self.vm_stack.len();
                        let iter_slot = self.vm_iters.len() as u32;
                        self.vm_iters.push((array, 0));
                        unsafe {
                            *self.vm_stack.get_unchecked_mut(len - 1) = Value::ArrayIter(iter_slot);
                        }
                    } else {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let value = pop_fast(&mut self.vm_stack);
                        self.iterable_callsite = callsite.map(|index| {
                            Callsite::Source(unsafe {
                                (&*code).names.get_unchecked(index as usize).clone()
                            })
                        });
                        let iterator = self.get_iterator(value);
                        self.iterable_callsite = None;
                        let iterator = iterator?;
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                        unsafe { push_unchecked(&mut self.vm_stack, iterator) };
                    }
                }
                Op::IterValue => {
                    let len = self.vm_stack.len();
                    let fast = if len > 0
                        && let Value::ArrayIter(iter_slot) =
                            unsafe { self.vm_stack.get_unchecked(len - 1) }
                    {
                        let (array, index) =
                            unsafe { self.vm_iters.get_unchecked_mut(*iter_slot as usize) };
                        if *index == u32::MAX {
                            Some(Value::Undefined)
                        } else if let Some(data) = unsafe { (&*heap_ptr).array_ref(array) } {
                            let idx = *index as usize;
                            if let Some(val) = data.values.get(idx) {
                                *index = (idx + 1) as u32;
                                Some(val.clone().filled())
                            } else {
                                *index = u32::MAX;
                                Some(Value::Undefined)
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(value) = fast {
                        unsafe { push_unchecked(&mut self.vm_stack, value) };
                    } else {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let iterator = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                        let value = self.iterator_step(&iterator)?.unwrap_or(Value::Undefined);
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                        unsafe { push_unchecked(&mut self.vm_stack, value) };
                    }
                }
                Op::IterStep(target) => {
                    let len = self.vm_stack.len();
                    let fast = if len > 0
                        && let Value::ArrayIter(iter_slot) =
                            unsafe { self.vm_stack.get_unchecked(len - 1) }
                    {
                        let (array, index) =
                            unsafe { self.vm_iters.get_unchecked_mut(*iter_slot as usize) };
                        if *index == u32::MAX {
                            Some(None)
                        } else if let Some(data) = unsafe { (&*heap_ptr).array_ref(array) } {
                            let idx = *index as usize;
                            if let Some(val) = data.values.get(idx) {
                                *index = (idx + 1) as u32;
                                Some(Some(val.clone().filled()))
                            } else {
                                *index = u32::MAX;
                                Some(None)
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    match fast {
                        Some(Some(value)) => unsafe { push_unchecked(&mut self.vm_stack, value) },
                        Some(None) => {
                            let target = target as usize;
                            if target <= op_pc {
                                self.steps += (op_pc + 1).saturating_sub(block_start).max(1) as u64;
                                if self.steps >= safepoint_at {
                                    safepoint_at = self.hit_safepoint(sampling, target)?;
                                }
                                reserve_headroom(&mut self.vm_stack, unsafe {
                                    (&*code).max_pushes as usize
                                });
                            }
                            pc = target;
                            block_start = pc;
                        }
                        None => {
                            unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                            let iterator =
                                self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                            match self.iterator_step(&iterator)? {
                                Some(value) => {
                                    reserve_headroom(&mut self.vm_stack, unsafe {
                                        (&*code).max_pushes as usize
                                    });
                                    unsafe { push_unchecked(&mut self.vm_stack, value) };
                                }
                                None => {
                                    pc = target as usize;
                                    block_start = pc;
                                }
                            }
                        }
                    }
                }
                Op::IterRest => {
                    let len = self.vm_stack.len();
                    let fast = if len > 0
                        && let Value::ArrayIter(iter_slot) =
                            unsafe { self.vm_stack.get_unchecked(len - 1) }
                    {
                        let (array, index) =
                            unsafe { self.vm_iters.get_unchecked_mut(*iter_slot as usize) };
                        if *index == u32::MAX {
                            Some(Vec::new())
                        } else if let Some(data) = unsafe { (&*heap_ptr).array_ref(array) } {
                            let idx = *index as usize;
                            *index = u32::MAX;
                            if idx < data.values.len() {
                                Some(
                                    data.values[idx..]
                                        .iter()
                                        .map(|v| v.clone().filled())
                                        .collect(),
                                )
                            } else {
                                Some(Vec::new())
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(rest) = fast {
                        let array = unsafe { &*heap_ptr }.alloc_array_fast(ArrayData::from(rest));
                        unsafe { push_unchecked(&mut self.vm_stack, Value::Array(array)) };
                    } else {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        let iterator = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                        let mut rest = Vec::new();
                        while let Some(value) = self.iterator_step(&iterator)? {
                            rest.push(value);
                        }
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                        unsafe { push_unchecked(&mut self.vm_stack, Self::new_array(rest)) };
                    }
                }
                Op::IterClose => {
                    let value = pop_fast(&mut self.vm_stack);
                    if let Value::ArrayIter(iter_slot) = value {
                        let slot = iter_slot as usize;
                        if slot < self.vm_iters.len() {
                            self.vm_iters.truncate(slot);
                        }
                    } else {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        self.iterator_close(&value)?;
                        value.fast_drop();
                        reserve_headroom(&mut self.vm_stack, unsafe {
                            (&*code).max_pushes as usize
                        });
                    }
                }
                Op::Return => {
                    let value = pop_fast(&mut self.vm_stack);
                    let frame = unsafe { self.frames.get_unchecked(top) };
                    if self.vm_handlers.len() == frame.handler_base as usize && !frame.construct() {
                        let mut finished = self.frames.pop().expect("a frame is running");
                        truncate_stack(&mut self.vm_stack, finished.return_base);
                        if self.vm_chains.len() > finished.chain_base as usize {
                            self.vm_chains.truncate(finished.chain_base as usize);
                        }
                        if self.vm_pending.len() > finished.pending_base as usize {
                            self.vm_pending.truncate(finished.pending_base as usize);
                        }
                        if self.vm_iters.len() > finished.iter_base as usize {
                            self.vm_iters.truncate(finished.iter_base as usize);
                        }
                        if finished.scopes > 0 {
                            Self::restore_scopes_fast(unsafe { &*heap_ptr }, &mut finished, 0);
                        }
                        if (finished.flags & FLAG_OWNS_SCOPE) != 0
                            && finished.escaped_scopes == 0
                            && let Some(scope) = finished.scope.take()
                        {
                            unsafe { &*heap_ptr }.free_scope_fast(scope);
                        }
                        let _ = finished.fast_drop_keep_unique_code();
                        if self.frames.len() == frame_floor {
                            return Ok(RunResult::Return(value));
                        }
                        self.vm_stack.push(value);
                    } else {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                        if let Some(result) = self.do_return(frame_floor, value)? {
                            return Ok(result);
                        }
                    }
                    reload_frame!();
                }
                other => {
                    unsafe { self.frames.get_unchecked_mut(top) }.pc = pc;
                    if let Some(result) = self.step_op(other, frame_floor)? {
                        return Ok(result);
                    }
                    reload_frame!();
                }
            }
        }
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(super) fn jit_hit_safepoint(
        &mut self,
        sampling: bool,
        pc: usize,
    ) -> Result<u64, ScriptError> {
        self.hit_safepoint(sampling, pc)
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(super) fn jit_call_plain(
        &mut self,
        heap: &super::heap::Heap,
        count: usize,
        method: bool,
    ) -> bool {
        self.call_plain(heap, count, method)
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(super) fn jit_call_fast_native(
        &mut self,
        heap: &super::heap::Heap,
        count: usize,
        method: bool,
    ) -> Result<FastNativeCall, ScriptError> {
        self.call_fast_native(heap, count, method)
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(super) fn jit_call_in_loop_stack(
        &mut self,
        count: usize,
        method: bool,
    ) -> Result<(), ScriptError> {
        let arguments = self.pop_arguments(count);
        let callee = pop_fast(&mut self.vm_stack);
        let this = if method {
            pop_fast(&mut self.vm_stack)
        } else {
            Value::Undefined
        };
        let _ = self.call_in_loop(callee, this, arguments)?;
        Ok(())
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline(always)]
    pub(super) fn jit_restore_scopes(heap: &super::heap::Heap, frame: &mut Frame) {
        Self::restore_scopes_fast(heap, frame, 0);
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(super) fn jit_return(
        &mut self,
        heap: &super::heap::Heap,
        top: usize,
        frame_floor: usize,
    ) -> Result<(Option<RunResult>, Option<Rc<Code>>), ScriptError> {
        let value = pop_fast(&mut self.vm_stack);
        let frame = unsafe { self.frames.get_unchecked(top) };
        if self.vm_handlers.len() == frame.handler_base as usize && !frame.construct() {
            let mut finished = self.frames.pop().expect("a frame is running");
            truncate_stack(&mut self.vm_stack, finished.return_base);
            if self.vm_chains.len() > finished.chain_base as usize {
                self.vm_chains.truncate(finished.chain_base as usize);
            }
            if self.vm_pending.len() > finished.pending_base as usize {
                self.vm_pending.truncate(finished.pending_base as usize);
            }
            if self.vm_iters.len() > finished.iter_base as usize {
                self.vm_iters.truncate(finished.iter_base as usize);
            }
            if finished.scopes > 0 {
                Self::restore_scopes_fast(heap, &mut finished, 0);
            }
            if (finished.flags & FLAG_OWNS_SCOPE) != 0
                && finished.escaped_scopes == 0
                && let Some(scope) = finished.scope.take()
            {
                heap.free_scope_fast(scope);
            }
            let keep_code = finished.fast_drop_keep_unique_code();
            if self.frames.len() == frame_floor {
                return Ok((Some(RunResult::Return(value)), keep_code));
            }
            self.vm_stack.push(value);
            return Ok((None, keep_code));
        }
        let code_keep = unsafe { self.frames.get_unchecked(top) }.code.clone();
        Ok((self.do_return(frame_floor, value)?, Some(code_keep)))
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline(always)]
    pub(super) fn jit_ic_get_value_only(
        &self,
        heap: &super::heap::Heap,
        receiver: &Value,
        cache: &super::object::PropIc,
    ) -> Option<Value> {
        match self.ic_get_value(heap, receiver, cache) {
            Some(super::object::IcGetHit::Value(v)) => Some(v),
            Some(super::object::IcGetHit::Missing) => Some(Value::Undefined),
            _ => None,
        }
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline(always)]
    pub(super) fn jit_try_fast_get_index(
        heap: &super::heap::Heap,
        object: &Value,
        key: &Value,
    ) -> Option<Value> {
        Self::try_fast_get_index(heap, object, key)
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline(always)]
    pub(super) fn jit_try_fast_set_index(
        heap: &super::heap::Heap,
        object: &Value,
        key: &Value,
        value: Value,
    ) -> Result<(), Value> {
        Self::try_fast_set_index(heap, object, key, value)
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_new_object(&self, heap: &super::heap::Heap) -> Value {
        let object = heap.alloc_object_fast(JsObject::with_prototype(
            Some(self.intrinsics.object_prototype.clone()),
            ObjectClass::Ordinary,
        ));
        Value::Object(object)
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) unsafe fn jit_make_object(
        &self,
        heap: &super::heap::Heap,
        code: &Code,
        template: u32,
        src: *const Value,
    ) -> Value {
        let (keys, cell) = unsafe { code.object_shapes.get_unchecked(template as usize) };
        let shape = cell
            .get_or_init(|| {
                let names: Vec<&str> = keys.iter().map(|name| &**name).collect();
                super::object::Shape::from_keys(&names)
            })
            .clone();
        let count = shape.len();
        let mut slots = Vec::with_capacity(count);
        for i in 0..count {
            slots.push(Slot::Data(unsafe { std::ptr::read(src.add(i)) }));
        }
        let object = heap.alloc_object_fast(JsObject::from_shape(
            shape,
            slots,
            Some(self.intrinsics.object_prototype.clone()),
        ));
        Value::Object(object)
    }

    #[inline]
    pub(super) fn try_split_regex_pop(
        &self,
        heap: &super::heap::Heap,
        code: &Code,
        pattern: u32,
        flags: u32,
        cache: u16,
        slot: &mut Value,
    ) -> bool {
        let Value::String(text) = &*slot else {
            return false;
        };
        if !text.is_ascii() {
            return false;
        }
        let intact = unsafe { heap.object_ref(&self.intrinsics.string_prototype) }
            .is_some_and(|o| o.string_split_intact(heap))
            && unsafe { heap.object_ref(&self.intrinsics.regexp_prototype) }
                .is_some_and(|o| o.regexp_split_intact(heap))
            && unsafe { heap.object_ref(&self.intrinsics.array_prototype) }
                .is_some_and(|o| o.array_pop_intact(heap));
        if !intact {
            return false;
        }
        let Some(cell) = code.split_regex_cache.get(cache as usize) else {
            return false;
        };
        let split_kind = *cell.get_or_init(|| {
            let pat = code.names.get(pattern as usize)?;
            let flg = code.names.get(flags as usize)?;
            let parsed = super::pattern::Flags::parse(flg).ok()?;
            let regex = super::pattern::Regex::new(pat, parsed).ok()?;
            regex.ascii_split().map(|(kind, _)| kind)
        });
        let Some(kind) = split_kind else {
            return false;
        };
        let bytes = text.as_bytes();
        let last_cut = match kind {
            super::pattern::AsciiSplit::One(b0) => bytes.iter().rposition(|&b| b == b0),
            super::pattern::AsciiSplit::Two(b0, b1) => {
                bytes.iter().rposition(|&b| b == b0 || b == b1)
            }
            super::pattern::AsciiSplit::Mask(mask) => {
                bytes.iter().rposition(|&b| (mask >> b) & 1 == 1)
            }
        };
        if let Some(pos) = last_cut {
            let tail = JsStr::new(&text[pos + 1..]);
            let old = std::mem::replace(slot, Value::String(tail));
            old.fast_drop();
        }
        true
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_make_closure(
        &mut self,
        heap: &super::heap::Heap,
        top: usize,
        index: u32,
    ) -> Value {
        let frame = unsafe { self.frames.get_unchecked_mut(top) };
        let child = unsafe { frame.code.children.get_unchecked(index as usize).clone() };
        let scope = if child.captures_nothing {
            None
        } else {
            frame.mark_scopes_escaped();
            frame.scope.clone()
        };
        let home = if child.kind == FunctionKind::Arrow {
            frame.closure.as_ref().and_then(|closure| closure.home())
        } else {
            None
        };
        let fast_template = (!child.is_generator
            && !child.is_async
            && matches!(child.kind, FunctionKind::Arrow | FunctionKind::Method))
        .then(|| child.function_template.get())
        .flatten();
        let lexical_this = child.uses_lexical_this.then(|| frame.this.clone());
        let closure = Rc::new(Closure::new(child.clone(), scope));
        if let Some(home) = home {
            closure.set_home_object(home);
        }
        if let Some(this) = lexical_this {
            closure.set_lexical_this(this);
        }
        if let Some(template) = fast_template {
            return Value::Object(heap.alloc_object_fast(JsObject::from_template(
                template,
                Some(self.intrinsics.function_prototype.clone()),
                ObjectClass::Function(closure),
            )));
        }
        self.function_object(closure)
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_get_global_fast(&self, key: &str) -> Option<Value> {
        let global = self.global.borrow();
        if global.accessors.is_empty() || !global.accessors.contains_key(key) {
            match global.values.get(key) {
                Some(Value::Uninitialized) | None => None,
                Some(value) => Some(value.resolve()),
            }
        } else {
            None
        }
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_typeof(&self, value: &Value) -> Value {
        Value::String(type_name(self.type_of(value)))
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_push_handler(&mut self, top: usize, sp_len: usize, catch: u32, finally: u32) {
        let frame = unsafe { self.frames.get_unchecked(top) };
        let stack_offset = sp_len - frame.stack_base;
        let chains = self.vm_chains.len() - frame.chain_base as usize;
        let iters = self.vm_iters.len() - frame.iter_base as usize;
        self.vm_handlers.push(Handler {
            catch,
            finally,
            stack: stack_offset,
            scopes: frame.scopes,
            chains,
            iters,
        });
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_pop_handler(&mut self, top: usize) {
        let base = unsafe { self.frames.get_unchecked(top) }.handler_base as usize;
        if self.vm_handlers.len() > base {
            self.vm_handlers.pop();
        }
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_completion_normal(&mut self) {
        self.vm_pending.push(Pending::Normal);
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_chain_start(&mut self, top: usize, sp_len: usize) {
        let offset = sp_len - unsafe { self.frames.get_unchecked(top) }.stack_base;
        self.vm_chains.push(offset);
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_chain_end(&mut self, top: usize) {
        let base = unsafe { self.frames.get_unchecked(top) }.chain_base as usize;
        if self.vm_chains.len() > base {
            self.vm_chains.pop();
        }
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_args_iter(&mut self, slot: &mut Value) {
        let Value::Array(array) = slot.clone() else {
            unreachable!("arguments arrive as an array");
        };
        let iter_slot = self.vm_iters.len() as u32;
        self.vm_iters.push((array, 0));
        *slot = Value::ArrayIter(iter_slot);
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_get_iter_fast(&mut self, heap: &super::heap::Heap, slot: &mut Value) -> bool {
        let Value::Array(array) = slot else {
            return false;
        };
        let plain = unsafe { heap.array_ref(array) }.is_some_and(|a| a.extra.is_none());
        let proto_ok = plain
            && unsafe { heap.object_ref(&self.intrinsics.array_prototype) }
                .is_some_and(|p| p.array_iterator_intact(&self.intrinsics.array_values));
        if !proto_ok {
            return false;
        }
        let array = array.clone();
        let iter_slot = self.vm_iters.len() as u32;
        self.vm_iters.push((array, 0));
        *slot = Value::ArrayIter(iter_slot);
        true
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_iter_value_fast(
        &mut self,
        heap: &super::heap::Heap,
        slot: &Value,
    ) -> Option<Value> {
        let Value::ArrayIter(iter_slot) = slot else {
            return None;
        };
        let (array, index) = unsafe { self.vm_iters.get_unchecked_mut(*iter_slot as usize) };
        if *index == u32::MAX {
            return Some(Value::Undefined);
        }
        let data = unsafe { heap.array_ref(array)? };
        let idx = *index as usize;
        if let Some(val) = data.values.get(idx) {
            *index = (idx + 1) as u32;
            Some(val.clone().filled())
        } else {
            *index = u32::MAX;
            Some(Value::Undefined)
        }
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_iter_step_fast(
        &mut self,
        heap: &super::heap::Heap,
        slot: &Value,
    ) -> Option<Option<Value>> {
        let Value::ArrayIter(iter_slot) = slot else {
            return None;
        };
        let (array, index) = unsafe { self.vm_iters.get_unchecked_mut(*iter_slot as usize) };
        if *index == u32::MAX {
            return Some(None);
        }
        let data = unsafe { heap.array_ref(array)? };
        let idx = *index as usize;
        if let Some(val) = data.values.get(idx) {
            *index = (idx + 1) as u32;
            Some(Some(val.clone().filled()))
        } else {
            *index = u32::MAX;
            Some(None)
        }
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_iter_close_array(&mut self, iter_slot: u32) {
        let slot = iter_slot as usize;
        if slot < self.vm_iters.len() {
            self.vm_iters.truncate(slot);
        }
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_iter_rest_fast(
        &mut self,
        heap: &super::heap::Heap,
        slot: &Value,
    ) -> Option<Value> {
        let Value::ArrayIter(iter_slot) = slot else {
            return None;
        };
        let (array, index) = unsafe { self.vm_iters.get_unchecked_mut(*iter_slot as usize) };
        let rest = if *index == u32::MAX {
            Vec::new()
        } else {
            let data = unsafe { heap.array_ref(array)? };
            let idx = *index as usize;
            *index = u32::MAX;
            if idx < data.values.len() {
                data.values[idx..]
                    .iter()
                    .map(|v| v.clone().filled())
                    .collect()
            } else {
                Vec::new()
            }
        };
        Some(Value::Array(heap.alloc_array_fast(ArrayData::from(rest))))
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_end_finally_normal(&mut self, top: usize) -> bool {
        let pending_base = unsafe { self.frames.get_unchecked(top) }.pending_base as usize;
        let len = self.vm_pending.len();
        if len <= pending_base {
            return true;
        }
        if matches!(
            unsafe { self.vm_pending.get_unchecked(len - 1) },
            Pending::Normal
        ) {
            self.vm_pending.pop();
            return true;
        }
        false
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_push_scope(
        &mut self,
        heap: &super::heap::Heap,
        top: usize,
        block: u32,
        is_var: bool,
    ) -> (*mut Value, usize) {
        let frame = unsafe { self.frames.get_unchecked_mut(top) };
        let layout = unsafe { frame.code.blocks.get_unchecked(block as usize).clone() };
        let parent = frame.scope.clone();
        let grows = layout.grows;
        let slots = heap.alloc_scope_slots(layout.names.len(), !is_var);
        let scope = heap.alloc_scope_fast(Scope {
            slots,
            parent,
            layout,
            extra: Grown::default(),
        });
        let cached = if grows {
            (std::ptr::null_mut(), 0)
        } else if let Some(s) = unsafe { heap.scope_at_mut(&scope, 0) } {
            (s.slots.as_mut_ptr(), s.slots.len())
        } else {
            (std::ptr::null_mut(), 0)
        };
        frame.scope = Some(scope);
        frame.scopes += 1;
        cached
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_pop_scope(&mut self, heap: &super::heap::Heap, top: usize) {
        let frame = unsafe { self.frames.get_unchecked_mut(top) };
        if frame.scopes > 0 {
            let depth = frame.scopes;
            let popped = frame.scope.take();
            let parent = popped.as_ref().and_then(|scope| {
                unsafe { heap.scope_at_ref(scope, 0) }
                    .map(|s| s.parent.clone())
                    .unwrap_or_else(|| scope.borrow().parent.clone())
            });
            if depth >= frame.escaped_scopes
                && let Some(scope) = popped
            {
                heap.free_scope_fast(scope);
            }
            frame.scope = parent;
            frame.scopes -= 1;
            if frame.escaped_scopes > frame.scopes + 1 {
                frame.escaped_scopes = frame.scopes + 1;
            }
        }
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    pub(super) fn jit_clone_scope(
        &mut self,
        heap: &super::heap::Heap,
        top: usize,
    ) -> (*mut Value, usize) {
        let frame = unsafe { self.frames.get_unchecked_mut(top) };
        let depth = frame.scopes;
        let current = frame.scope.take().expect("a scope to copy");
        let cloned = if let Some(scope) = unsafe { heap.scope_at_ref(&current, 0) } {
            Scope {
                slots: heap.clone_scope_slots(&scope.slots),
                parent: scope.parent.clone(),
                layout: scope.layout.clone(),
                extra: scope.extra.clone(),
            }
        } else {
            let scope = current.borrow();
            Scope {
                slots: heap.clone_scope_slots(&scope.slots),
                parent: scope.parent.clone(),
                layout: scope.layout.clone(),
                extra: scope.extra.clone(),
            }
        };
        let grows = cloned.layout.grows;
        if depth > 0 && depth >= frame.escaped_scopes {
            heap.free_scope_fast(current);
        }
        if frame.escaped_scopes > depth {
            frame.escaped_scopes = depth;
        }
        let next = heap.alloc_scope_fast(cloned);
        let cached = if grows {
            (std::ptr::null_mut(), 0)
        } else if let Some(s) = unsafe { heap.scope_at_mut(&next, 0) } {
            (s.slots.as_mut_ptr(), s.slots.len())
        } else {
            (std::ptr::null_mut(), 0)
        };
        frame.scope = Some(next);
        cached
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn jit_get_prop_ic_or_slow(
        &mut self,
        heap: &super::heap::Heap,
        top: usize,
        op_pc: usize,
        next_pc: usize,
        name: u32,
        ic: u32,
        cache: &super::object::PropIc,
        mode: u32,
        safepoint_at: &mut u64,
        sampling: bool,
    ) -> Result<(), ScriptError> {
        use super::object::IcGetHit;
        let op = unsafe { *self.frames.get_unchecked(top).code.ops.get_unchecked(op_pc) };
        let is_method = matches!(mode, 2 | 4);
        let recv_opt = match op {
            Op::GetProp { .. } => None,
            Op::GetRegProp { reg, .. } | Op::GetRegMethod { reg, .. } => {
                let reg_base = unsafe { self.frames.get_unchecked(top) }.reg_base;
                let value = unsafe { self.vm_stack.get_unchecked(reg_base + reg as usize) };
                if matches!(value, Value::Uninitialized) {
                    unsafe { self.frames.get_unchecked_mut(top) }.pc = next_pc;
                    let key = self.frames[top].code.layout.names[reg as usize].clone();
                    return Err(
                        self.reference_error(format!("cannot access {key} before initialization"))
                    );
                }
                Some(value.clone())
            }
            Op::GetLocalProp { depth, index, .. } | Op::GetLocalMethod { depth, index, .. } => {
                let fast = unsafe {
                    self.frames
                        .get_unchecked(top)
                        .scope
                        .as_ref()
                        .and_then(|start| heap.scope_at_ref(start, depth))
                        .and_then(|scope| match scope.slots.get(index as usize) {
                            Some(Value::Binding(cell) | Value::ConstBinding(cell)) => {
                                let inner = cell.borrow();
                                (!matches!(*inner, Value::Uninitialized)).then(|| inner.clone())
                            }
                            Some(Value::Uninitialized) | None => None,
                            Some(v) => Some(v.clone()),
                        })
                };
                match fast {
                    Some(v) => Some(v),
                    None => {
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = next_pc;
                        let key = self.local_layout_name(top, depth, index);
                        Some(self.read_local_named(top, depth, index, &key)?)
                    }
                }
            }
            _ => unreachable!(),
        };
        let hit = match &recv_opt {
            Some(recv) => self.ic_get_value(heap, recv, cache),
            None => self
                .vm_stack
                .last()
                .and_then(|recv| self.ic_get_value(heap, recv, cache)),
        };
        match hit {
            Some(IcGetHit::Value(value)) => {
                if let Some(recv) = recv_opt {
                    if is_method {
                        self.vm_stack.push(recv);
                    } else {
                        recv.fast_drop();
                    }
                    self.vm_stack.push(value);
                } else {
                    let len = self.vm_stack.len();
                    let old = std::mem::replace(
                        unsafe { self.vm_stack.get_unchecked_mut(len - 1) },
                        value,
                    );
                    old.fast_drop();
                }
                Ok(())
            }
            Some(IcGetHit::Missing) => {
                if let Some(recv) = recv_opt {
                    if is_method {
                        self.vm_stack.push(recv);
                    } else {
                        recv.fast_drop();
                    }
                    self.vm_stack.push(Value::Undefined);
                } else {
                    let len = self.vm_stack.len();
                    let old = std::mem::replace(
                        unsafe { self.vm_stack.get_unchecked_mut(len - 1) },
                        Value::Undefined,
                    );
                    old.fast_drop();
                }
                Ok(())
            }
            Some(IcGetHit::Getter(getter)) => {
                if let Some(recv) = recv_opt {
                    if is_method {
                        let recv2 = recv.clone();
                        self.vm_stack.push(recv);
                        self.vm_stack.push(recv2);
                    } else {
                        self.vm_stack.push(recv);
                    }
                }
                self.vm_stack.push(getter);
                self.steps += 1;
                if self.steps >= *safepoint_at {
                    *safepoint_at = self.hit_safepoint(sampling, op_pc)?;
                }
                unsafe { self.frames.get_unchecked_mut(top) }.pc = next_pc;
                let _ = self.call_ic_getter(heap)?;
                Ok(())
            }
            None => {
                if let Some(recv) = recv_opt {
                    if is_method {
                        let recv2 = recv.clone();
                        self.vm_stack.push(recv);
                        self.vm_stack.push(recv2);
                    } else {
                        self.vm_stack.push(recv);
                    }
                }
                unsafe { self.frames.get_unchecked_mut(top) }.pc = next_pc;
                let _ = self.get_prop_slow(name, ic)?;
                Ok(())
            }
        }
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    pub(super) fn jit_set_prop_ic_or_slow(
        &mut self,
        heap: &super::heap::Heap,
        name: u32,
        ic: u32,
        cache: &super::object::PropIc,
        pop: bool,
    ) -> Result<(), ScriptError> {
        if pop {
            let value = pop_fast(&mut self.vm_stack);
            let object = pop_fast(&mut self.vm_stack);
            let missed = match &object {
                Value::Object(handle) => match unsafe { heap.object_mut(handle) } {
                    Some(target) => target.ic_set_fast(handle, heap, cache, value),
                    None => Err(value),
                },
                _ => Err(value),
            };
            if let Err(value) = missed {
                self.set_prop_slow_with(object, value, name, ic, false)?;
            } else {
                object.fast_drop();
            }
        } else {
            let len = self.vm_stack.len();
            let hit = len >= 2
                && match unsafe { self.vm_stack.get_unchecked(len - 2) } {
                    Value::Object(handle) => {
                        let value = unsafe { self.vm_stack.get_unchecked(len - 1).clone() };
                        unsafe { heap.object_mut(handle) }.is_some_and(|target| {
                            target.ic_set_fast(handle, heap, cache, value).is_ok()
                        })
                    }
                    _ => false,
                };
            if hit {
                let value = pop_fast(&mut self.vm_stack);
                let old =
                    std::mem::replace(unsafe { self.vm_stack.get_unchecked_mut(len - 2) }, value);
                old.fast_drop();
            } else {
                self.set_prop_slow(name, ic)?;
            }
        }
        Ok(())
    }

    #[cfg(all(
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn jit_exec_op(
        &mut self,
        heap: &super::heap::Heap,
        op: Op,
        op_pc: usize,
        next_pc: usize,
        frame_floor: usize,
        safepoint_at: &mut u64,
        sampling: bool,
    ) -> Result<Option<RunResult>, ScriptError> {
        let top = self.frames.len() - 1;
        let reg_base = unsafe { self.frames.get_unchecked(top) }.reg_base;
        let code_ptr: *const Code = Rc::as_ptr(&unsafe { self.frames.get_unchecked(top) }.code);
        match op {
            Op::UpdateReg {
                reg,
                name,
                increment,
                prefix,
            } => {
                self.update_reg_slow(top, reg, name, increment, prefix)?;
                Ok(None)
            }
            Op::UpdateRegPop { reg, increment } => {
                let name = unsafe { (&*code_ptr).layout.names[reg as usize].clone() };
                self.update_reg_slow_named(top, reg, &name, increment, false)?;
                pop_fast(&mut self.vm_stack).fast_drop();
                Ok(None)
            }
            Op::UninitRegs { index, count } => {
                let start = reg_base + index as usize;
                let end = start + count as usize;
                for slot in &mut self.vm_stack[start..end] {
                    let old = std::mem::replace(slot, Value::Uninitialized);
                    old.fast_drop();
                }
                Ok(None)
            }
            Op::GetLocal { depth, index, name } => {
                let fast = unsafe {
                    self.frames
                        .get_unchecked(top)
                        .scope
                        .as_ref()
                        .and_then(|start| heap.scope_at_ref(start, depth))
                        .and_then(|scope| match scope.slots.get(index as usize) {
                            Some(Value::Binding(cell) | Value::ConstBinding(cell)) => {
                                let inner = cell.borrow();
                                (!matches!(*inner, Value::Uninitialized)).then(|| inner.clone())
                            }
                            Some(Value::Uninitialized) | None => None,
                            Some(v) => Some(v.clone()),
                        })
                };
                let value = match fast {
                    Some(v) => v,
                    None => self.read_local(top, depth, index, name)?,
                };
                self.vm_stack.push(value);
                Ok(None)
            }
            Op::SetLocal { depth, index, name } => {
                let value = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                let written = unsafe {
                    self.frames
                        .get_unchecked(top)
                        .scope
                        .as_ref()
                        .and_then(|start| heap.scope_at_mut(start, depth))
                        .and_then(|scope| match scope.slots.get_mut(index as usize) {
                            Some(
                                Value::Binding(_) | Value::ConstBinding(_) | Value::Uninitialized,
                            )
                            | None => None,
                            Some(slot) => Some(slot),
                        })
                };
                if let Some(slot) = written {
                    let old = std::mem::replace(slot, value);
                    old.fast_drop();
                } else {
                    self.write_local(top, depth, index, name, value)?;
                }
                Ok(None)
            }
            Op::SetLocalPop { depth, index } => {
                let value = pop_fast(&mut self.vm_stack);
                let written = unsafe {
                    self.frames
                        .get_unchecked(top)
                        .scope
                        .as_ref()
                        .and_then(|start| heap.scope_at_mut(start, depth))
                        .and_then(|scope| match scope.slots.get_mut(index as usize) {
                            Some(
                                Value::Binding(_) | Value::ConstBinding(_) | Value::Uninitialized,
                            )
                            | None => None,
                            Some(slot) => Some(slot),
                        })
                };
                if let Some(slot) = written {
                    let old = std::mem::replace(slot, value);
                    old.fast_drop();
                } else {
                    let name = self.local_layout_name(top, depth, index);
                    self.write_local_named(top, depth, index, &name, value)?;
                }
                Ok(None)
            }
            Op::InitLocal { depth, index } => {
                let value = pop_fast(&mut self.vm_stack);
                if let Some(scope) = unsafe {
                    self.frames
                        .get_unchecked(top)
                        .scope
                        .as_ref()
                        .and_then(|start| heap.scope_at_mut(start, depth))
                } {
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
                } else {
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
                Ok(None)
            }
            Op::UpdateLocal {
                depth,
                index,
                name,
                increment,
                prefix,
            } => {
                let fast = unsafe {
                    self.frames
                        .get_unchecked(top)
                        .scope
                        .as_ref()
                        .and_then(|start| heap.scope_at_mut(start, depth))
                        .and_then(|scope| {
                            let Value::Number(slot) = scope.slots.get_mut(index as usize)? else {
                                return None;
                            };
                            let old = *slot;
                            let new = if increment { old + 1.0 } else { old - 1.0 };
                            *slot = new;
                            Some(Value::Number(if prefix { new } else { old }))
                        })
                };
                match fast {
                    Some(v) => self.vm_stack.push(v),
                    None => self.update_local_slow(top, depth, index, name, increment, prefix)?,
                }
                Ok(None)
            }
            Op::UpdateLocalPop {
                depth,
                index,
                increment,
            } => {
                let fast = unsafe {
                    self.frames
                        .get_unchecked(top)
                        .scope
                        .as_ref()
                        .and_then(|start| heap.scope_at_mut(start, depth))
                        .is_some_and(|scope| {
                            if let Some(Value::Number(slot)) = scope.slots.get_mut(index as usize) {
                                *slot = if increment { *slot + 1.0 } else { *slot - 1.0 };
                                true
                            } else {
                                false
                            }
                        })
                };
                if !fast {
                    let name = self.local_layout_name(top, depth, index);
                    self.update_local_slow_named(top, depth, index, &name, increment, false)?;
                    pop_fast(&mut self.vm_stack).fast_drop();
                }
                Ok(None)
            }
            Op::GetLocalProp { name, ic, .. } => {
                let cache = unsafe { (&*code_ptr).ics.get_unchecked(ic as usize) };
                self.jit_get_prop_ic_or_slow(
                    heap,
                    top,
                    op_pc,
                    next_pc,
                    name,
                    ic,
                    cache,
                    3,
                    safepoint_at,
                    sampling,
                )?;
                Ok(None)
            }
            Op::GetLocalMethod { name, ic, .. } => {
                let cache = unsafe { (&*code_ptr).ics.get_unchecked(ic as usize) };
                self.jit_get_prop_ic_or_slow(
                    heap,
                    top,
                    op_pc,
                    next_pc,
                    name,
                    ic,
                    cache,
                    4,
                    safepoint_at,
                    sampling,
                )?;
                Ok(None)
            }
            Op::GetGlobal(name) => {
                let key: &str = unsafe { (&*code_ptr).names.get_unchecked(name as usize) };
                let fast = {
                    let global = self.global.borrow();
                    if global.accessors.is_empty() || !global.accessors.contains_key(key) {
                        match global.values.get(key) {
                            Some(Value::Uninitialized) | None => None,
                            Some(v) => Some(v.resolve()),
                        }
                    } else {
                        None
                    }
                };
                let value = match fast {
                    Some(v) => v,
                    None => {
                        let key = unsafe { (&*code_ptr).names[name as usize].clone() };
                        self.get_global(&key)?
                    }
                };
                self.vm_stack.push(value);
                Ok(None)
            }
            Op::This => {
                let frame = unsafe { self.frames.get_unchecked(top) };
                if matches!(frame.this, Value::Uninitialized) {
                    return Err(
                        self.reference_error("must call super constructor before accessing this")
                    );
                }
                let this = frame.this.clone();
                self.vm_stack.push(this);
                Ok(None)
            }
            Op::TypeOf => {
                let value = pop_fast(&mut self.vm_stack);
                let kind = self.type_of(&value);
                value.fast_drop();
                self.vm_stack.push(Value::String(type_name(kind)));
                Ok(None)
            }
            Op::ToPropertyKey => {
                let value = pop_fast(&mut self.vm_stack);
                let key = self.coerce_key(value)?;
                self.vm_stack.push(Value::String(key.into()));
                Ok(None)
            }
            Op::ToIndexKey => {
                let len = self.vm_stack.len();
                if len >= 2
                    && matches!(
                        self.vm_stack[len - 2],
                        Value::Undefined | Value::Null | Value::Uninitialized
                    )
                {
                    return Err(self.type_error("cannot read properties of null or undefined"));
                }
                let value = pop_fast(&mut self.vm_stack);
                let key = self.coerce_key(value)?;
                self.vm_stack.push(Value::String(key.into()));
                Ok(None)
            }
            Op::Negate => {
                let value = pop_fast(&mut self.vm_stack);
                let result = self.negate_numeric(value)?;
                self.vm_stack.push(result);
                Ok(None)
            }
            Op::Plus => {
                let value = pop_fast(&mut self.vm_stack);
                let number = self.coerce_number(value)?;
                self.vm_stack.push(Value::Number(number));
                Ok(None)
            }
            Op::Increment | Op::Decrement => {
                let value = pop_fast(&mut self.vm_stack);
                let delta = if matches!(op, Op::Increment) {
                    1.0
                } else {
                    -1.0
                };
                let delta_i = if matches!(op, Op::Increment) { 1 } else { -1 };
                let result = match self.coerce_numeric(value)? {
                    Value::BigInt(v) => {
                        Value::BigInt(Rc::new(v.add(&super::bigint::BigInt::from_i64(delta_i))))
                    }
                    numeric => Value::Number(self.coerce_number(numeric)? + delta),
                };
                self.vm_stack.push(result);
                Ok(None)
            }
            Op::Binary(operator) => {
                let right = pop_fast(&mut self.vm_stack);
                let left = pop_fast(&mut self.vm_stack);
                let result = self.binary(operator, left, right)?;
                self.vm_stack.push(result);
                Ok(None)
            }
            Op::BinaryRegs {
                left,
                right,
                op: operator,
            } => {
                let left_ref = unsafe { self.vm_stack.get_unchecked(reg_base + left as usize) };
                if matches!(left_ref, Value::Uninitialized) {
                    let name = unsafe { (&*code_ptr).layout.names[left as usize].clone() };
                    return Err(
                        self.reference_error(format!("cannot access {name} before initialization"))
                    );
                }
                let right_ref = unsafe { self.vm_stack.get_unchecked(reg_base + right as usize) };
                if matches!(right_ref, Value::Uninitialized) {
                    let name = unsafe { (&*code_ptr).layout.names[right as usize].clone() };
                    return Err(
                        self.reference_error(format!("cannot access {name} before initialization"))
                    );
                }
                let l = left_ref.clone();
                let r = right_ref.clone();
                let result = self.binary(operator, l, r)?;
                self.vm_stack.push(result);
                Ok(None)
            }
            Op::JumpIfNotCompare {
                op: operator,
                target,
            } => {
                let right = pop_fast(&mut self.vm_stack);
                let left = pop_fast(&mut self.vm_stack);
                let result = self.binary(operator, left, right)?;
                let passed = result.truthy();
                result.fast_drop();
                if !passed {
                    let target = target as usize;
                    if target <= op_pc {
                        self.steps += 1;
                        if self.steps >= *safepoint_at {
                            *safepoint_at = self.hit_safepoint(sampling, target)?;
                        }
                    }
                    unsafe { self.frames.get_unchecked_mut(top) }.pc = target;
                }
                Ok(None)
            }
            Op::JumpIfNotCompareRegs {
                left,
                right,
                op: operator,
                target,
            } => {
                let left_ref = unsafe { self.vm_stack.get_unchecked(reg_base + left as usize) };
                if matches!(left_ref, Value::Uninitialized) {
                    let name = unsafe { (&*code_ptr).layout.names[left as usize].clone() };
                    return Err(
                        self.reference_error(format!("cannot access {name} before initialization"))
                    );
                }
                let right_ref = unsafe { self.vm_stack.get_unchecked(reg_base + right as usize) };
                if matches!(right_ref, Value::Uninitialized) {
                    let name = unsafe { (&*code_ptr).layout.names[right as usize].clone() };
                    return Err(
                        self.reference_error(format!("cannot access {name} before initialization"))
                    );
                }
                let l = left_ref.clone();
                let r = right_ref.clone();
                let result = self.binary(operator, l, r)?;
                let passed = result.truthy();
                result.fast_drop();
                if !passed {
                    let target = target as usize;
                    if target <= op_pc {
                        self.steps += 1;
                        if self.steps >= *safepoint_at {
                            *safepoint_at = self.hit_safepoint(sampling, target)?;
                        }
                    }
                    unsafe { self.frames.get_unchecked_mut(top) }.pc = target;
                }
                Ok(None)
            }
            Op::NewObject => {
                let object = heap.alloc_object_fast(JsObject::with_prototype(
                    Some(self.intrinsics.object_prototype.clone()),
                    ObjectClass::Ordinary,
                ));
                self.vm_stack.push(Value::Object(object));
                Ok(None)
            }
            Op::MakeObject(template) => {
                let (keys, cell) =
                    unsafe { (&*code_ptr).object_shapes.get_unchecked(template as usize) };
                let shape = cell
                    .get_or_init(|| {
                        let names: Vec<&str> = keys.iter().map(|name| &**name).collect();
                        super::object::Shape::from_keys(&names)
                    })
                    .clone();
                let count = shape.len();
                let start = self.vm_stack.len() - count;
                let mut slots = Vec::with_capacity(count);
                unsafe {
                    let src = self.vm_stack.as_ptr().add(start);
                    for i in 0..count {
                        slots.push(Slot::Data(std::ptr::read(src.add(i))));
                    }
                    self.vm_stack.set_len(start);
                }
                let object = heap.alloc_object_fast(JsObject::from_shape(
                    shape,
                    slots,
                    Some(self.intrinsics.object_prototype.clone()),
                ));
                self.vm_stack.push(Value::Object(object));
                Ok(None)
            }
            Op::NewArray => {
                let array = heap.alloc_array_fast(ArrayData::default());
                self.vm_stack.push(Value::Array(array));
                Ok(None)
            }
            Op::MakeArray(count) => {
                let count = count as usize;
                let values = if count == 0 {
                    Vec::new()
                } else {
                    let start = self.vm_stack.len() - count;
                    self.vm_stack.split_off(start)
                };
                let array = heap.alloc_array_fast(ArrayData::from(values));
                self.vm_stack.push(Value::Array(array));
                Ok(None)
            }
            Op::ArrayPush => {
                let value = pop_fast(&mut self.vm_stack);
                if let Some(Value::Array(array)) = self.vm_stack.last() {
                    if let Some(data) = unsafe { heap.array_mut(array) } {
                        data.values.push(value);
                    } else {
                        array.borrow_mut().push(value);
                    }
                } else {
                    value.fast_drop();
                }
                Ok(None)
            }
            Op::DefineField(name) => {
                let value = pop_fast(&mut self.vm_stack);
                if let Some(Value::Object(object)) = self.vm_stack.last() {
                    let key: &str = unsafe { (&*code_ptr).names.get_unchecked(name as usize) };
                    if let Some(target) = unsafe { heap.object_mut(object) } {
                        target.define_key(key, Property::data(value));
                    } else {
                        object.borrow_mut().define_key(key, Property::data(value));
                    }
                } else {
                    value.fast_drop();
                }
                Ok(None)
            }
            Op::Closure(index) => {
                let child = unsafe { (&*code_ptr).children.get_unchecked(index as usize).clone() };
                let frame = unsafe { self.frames.get_unchecked_mut(top) };
                let scope = if child.captures_nothing {
                    None
                } else {
                    frame.mark_scopes_escaped();
                    frame.scope.clone()
                };
                let home = if child.kind == FunctionKind::Arrow {
                    frame.closure.as_ref().and_then(|closure| closure.home())
                } else {
                    None
                };
                let lexical_this = child.uses_lexical_this.then(|| frame.this.clone());
                let closure = Rc::new(Closure::new(child, scope));
                if let Some(home) = home {
                    closure.set_home_object(home);
                }
                if let Some(this) = lexical_this {
                    closure.set_lexical_this(this);
                }
                let function = self.function_object(closure);
                self.vm_stack.push(function);
                Ok(None)
            }
            Op::GetIndex => {
                let len = self.vm_stack.len();
                if len >= 2
                    && let Some(value) = Self::try_fast_get_index(
                        heap,
                        unsafe { self.vm_stack.get_unchecked(len - 2) },
                        unsafe { self.vm_stack.get_unchecked(len - 1) },
                    )
                {
                    let key = pop_fast(&mut self.vm_stack);
                    key.fast_drop();
                    let old = std::mem::replace(
                        unsafe { self.vm_stack.get_unchecked_mut(len - 2) },
                        value,
                    );
                    old.fast_drop();
                } else {
                    let key = pop_fast(&mut self.vm_stack);
                    let object = pop_fast(&mut self.vm_stack);
                    match self.get_index_tail(&object, key)? {
                        super::operations::GetOutcome::Value(v) => self.vm_stack.push(v),
                        super::operations::GetOutcome::Call {
                            callee,
                            this,
                            arguments,
                        } => {
                            let _ = self.call_in_loop(callee, this, arguments)?;
                        }
                    }
                }
                Ok(None)
            }
            Op::GetRegIndex { array, index } => {
                let arr_ref = unsafe { self.vm_stack.get_unchecked(reg_base + array as usize) };
                if matches!(arr_ref, Value::Uninitialized) {
                    let name = unsafe { (&*code_ptr).layout.names[array as usize].clone() };
                    return Err(
                        self.reference_error(format!("cannot access {name} before initialization"))
                    );
                }
                let idx_ref = unsafe { self.vm_stack.get_unchecked(reg_base + index as usize) };
                if matches!(idx_ref, Value::Uninitialized) {
                    let name = unsafe { (&*code_ptr).layout.names[index as usize].clone() };
                    return Err(
                        self.reference_error(format!("cannot access {name} before initialization"))
                    );
                }
                if let Some(value) = Self::try_fast_get_index(heap, arr_ref, idx_ref) {
                    self.vm_stack.push(value);
                } else {
                    let object = arr_ref.clone();
                    let key = idx_ref.clone();
                    match self.get_index_tail(&object, key)? {
                        super::operations::GetOutcome::Value(v) => self.vm_stack.push(v),
                        super::operations::GetOutcome::Call {
                            callee,
                            this,
                            arguments,
                        } => {
                            let _ = self.call_in_loop(callee, this, arguments)?;
                        }
                    }
                }
                Ok(None)
            }
            Op::SetIndex => {
                let len = self.vm_stack.len();
                let hit = len >= 3 && {
                    let value = unsafe { self.vm_stack.get_unchecked(len - 1).clone() };
                    Self::try_fast_set_index(
                        heap,
                        unsafe { self.vm_stack.get_unchecked(len - 3) },
                        unsafe { self.vm_stack.get_unchecked(len - 2) },
                        value,
                    )
                    .is_ok()
                };
                if hit {
                    let value = pop_fast(&mut self.vm_stack);
                    let key = pop_fast(&mut self.vm_stack);
                    key.fast_drop();
                    let old = std::mem::replace(
                        unsafe { self.vm_stack.get_unchecked_mut(len - 3) },
                        value,
                    );
                    old.fast_drop();
                } else {
                    let value = pop_fast(&mut self.vm_stack);
                    let key = pop_fast(&mut self.vm_stack);
                    let object = pop_fast(&mut self.vm_stack);
                    let strict = unsafe { (&*code_ptr).strict };
                    self.set_index(&object, key, value.clone(), strict)?;
                    object.fast_drop();
                    self.vm_stack.push(value);
                }
                Ok(None)
            }
            Op::SetIndexPop => {
                let value = pop_fast(&mut self.vm_stack);
                let key = pop_fast(&mut self.vm_stack);
                let object = pop_fast(&mut self.vm_stack);
                match Self::try_fast_set_index(heap, &object, &key, value) {
                    Ok(()) => {
                        key.fast_drop();
                        object.fast_drop();
                    }
                    Err(value) => {
                        let strict = unsafe { (&*code_ptr).strict };
                        self.set_index(&object, key, value, strict)?;
                        object.fast_drop();
                    }
                }
                Ok(None)
            }
            Op::PushScope(block) => {
                let layout = unsafe { (&*code_ptr).blocks.get_unchecked(block as usize).clone() };
                let frame = unsafe { self.frames.get_unchecked_mut(top) };
                let parent = frame.scope.clone();
                let slots = heap.alloc_scope_slots(layout.names.len(), true);
                let scope = heap.alloc_scope_fast(Scope {
                    slots,
                    parent,
                    layout,
                    extra: Grown::default(),
                });
                frame.scope = Some(scope);
                frame.scopes += 1;
                Ok(None)
            }
            Op::PushVarScope(block) => {
                let layout = unsafe { (&*code_ptr).blocks.get_unchecked(block as usize).clone() };
                let frame = unsafe { self.frames.get_unchecked_mut(top) };
                let parent = frame.scope.clone();
                let slots = heap.alloc_scope_slots(layout.names.len(), false);
                let scope = heap.alloc_scope_fast(Scope {
                    slots,
                    parent,
                    layout,
                    extra: Grown::default(),
                });
                frame.scope = Some(scope);
                frame.scopes += 1;
                Ok(None)
            }
            Op::PopScope => {
                self.jit_pop_scope(heap, top);
                Ok(None)
            }
            Op::CloneScope => {
                let _ = self.jit_clone_scope(heap, top);
                Ok(None)
            }
            Op::ArgumentsIterator => {
                let len = self.vm_stack.len();
                let slot = unsafe { self.vm_stack.get_unchecked_mut(len - 1) };
                let Value::Array(array) = slot.clone() else {
                    unreachable!("arguments arrive as an array");
                };
                let iter_slot = self.vm_iters.len() as u32;
                self.vm_iters.push((array, 0));
                *slot = Value::ArrayIter(iter_slot);
                Ok(None)
            }
            Op::GetIterator(callsite) => {
                let fast = if let Some(Value::Array(array)) = self.vm_stack.last() {
                    let plain_array =
                        unsafe { heap.array_ref(array) }.is_some_and(|a| a.extra.is_none());
                    let proto_ok = plain_array
                        && unsafe { heap.object_ref(&self.intrinsics.array_prototype) }
                            .is_some_and(|p| {
                                p.array_iterator_intact(&self.intrinsics.array_values)
                            });
                    proto_ok.then(|| array.clone())
                } else {
                    None
                };
                if let Some(array) = fast {
                    let len = self.vm_stack.len();
                    let iter_slot = self.vm_iters.len() as u32;
                    self.vm_iters.push((array, 0));
                    unsafe {
                        *self.vm_stack.get_unchecked_mut(len - 1) = Value::ArrayIter(iter_slot);
                    }
                } else {
                    let value = pop_fast(&mut self.vm_stack);
                    self.iterable_callsite = callsite.map(|index| {
                        Callsite::Source(unsafe {
                            (&*code_ptr).names.get_unchecked(index as usize).clone()
                        })
                    });
                    let iterator = self.get_iterator(value);
                    self.iterable_callsite = None;
                    self.vm_stack.push(iterator?);
                }
                Ok(None)
            }
            Op::IterValue => {
                let len = self.vm_stack.len();
                let fast = if len > 0
                    && let Value::ArrayIter(iter_slot) =
                        unsafe { self.vm_stack.get_unchecked(len - 1) }
                {
                    let (array, index) =
                        unsafe { self.vm_iters.get_unchecked_mut(*iter_slot as usize) };
                    if *index == u32::MAX {
                        Some(Value::Undefined)
                    } else if let Some(data) = unsafe { heap.array_ref(array) } {
                        let idx = *index as usize;
                        if let Some(val) = data.values.get(idx) {
                            *index = (idx + 1) as u32;
                            Some(val.clone().filled())
                        } else {
                            *index = u32::MAX;
                            Some(Value::Undefined)
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                let value = match fast {
                    Some(v) => v,
                    None => {
                        let iterator = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                        self.iterator_step(&iterator)?.unwrap_or(Value::Undefined)
                    }
                };
                self.vm_stack.push(value);
                Ok(None)
            }
            Op::IterStep(target) => {
                let len = self.vm_stack.len();
                let fast = if len > 0
                    && let Value::ArrayIter(iter_slot) =
                        unsafe { self.vm_stack.get_unchecked(len - 1) }
                {
                    let (array, index) =
                        unsafe { self.vm_iters.get_unchecked_mut(*iter_slot as usize) };
                    if *index == u32::MAX {
                        Some(None)
                    } else if let Some(data) = unsafe { heap.array_ref(array) } {
                        let idx = *index as usize;
                        if let Some(val) = data.values.get(idx) {
                            *index = (idx + 1) as u32;
                            Some(Some(val.clone().filled()))
                        } else {
                            *index = u32::MAX;
                            Some(None)
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                match fast {
                    Some(Some(v)) => self.vm_stack.push(v),
                    Some(None) => {
                        let target = target as usize;
                        if target <= op_pc {
                            self.steps += 1;
                            if self.steps >= *safepoint_at {
                                *safepoint_at = self.hit_safepoint(sampling, target)?;
                            }
                        }
                        unsafe { self.frames.get_unchecked_mut(top) }.pc = target;
                    }
                    None => {
                        let iterator = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                        match self.iterator_step(&iterator)? {
                            Some(v) => self.vm_stack.push(v),
                            None => {
                                unsafe { self.frames.get_unchecked_mut(top) }.pc = target as usize
                            }
                        }
                    }
                }
                Ok(None)
            }
            Op::IterRest => {
                let len = self.vm_stack.len();
                let fast = if len > 0
                    && let Value::ArrayIter(iter_slot) =
                        unsafe { self.vm_stack.get_unchecked(len - 1) }
                {
                    let (array, index) =
                        unsafe { self.vm_iters.get_unchecked_mut(*iter_slot as usize) };
                    if *index == u32::MAX {
                        Some(Vec::new())
                    } else if let Some(data) = unsafe { heap.array_ref(array) } {
                        let idx = *index as usize;
                        *index = u32::MAX;
                        if idx < data.values.len() {
                            Some(
                                data.values[idx..]
                                    .iter()
                                    .map(|v| v.clone().filled())
                                    .collect(),
                            )
                        } else {
                            Some(Vec::new())
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(rest) = fast {
                    let array = heap.alloc_array_fast(ArrayData::from(rest));
                    self.vm_stack.push(Value::Array(array));
                } else {
                    let iterator = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                    let mut rest = Vec::new();
                    while let Some(v) = self.iterator_step(&iterator)? {
                        rest.push(v);
                    }
                    self.vm_stack.push(Self::new_array(rest));
                }
                Ok(None)
            }
            Op::IterClose => {
                let value = pop_fast(&mut self.vm_stack);
                if let Value::ArrayIter(iter_slot) = value {
                    let slot = iter_slot as usize;
                    if slot < self.vm_iters.len() {
                        self.vm_iters.truncate(slot);
                    }
                } else {
                    self.iterator_close(&value)?;
                    value.fast_drop();
                }
                Ok(None)
            }
            Op::Uninitialize { index, count } => {
                if let Some(scope_ref) = unsafe { self.frames.get_unchecked(top) }.scope.as_ref() {
                    if let Some(scope) = unsafe { heap.scope_at_mut(scope_ref, 0) } {
                        let end = (index as usize + count as usize).min(scope.slots.len());
                        for slot in &mut scope.slots[index as usize..end] {
                            let old = std::mem::replace(slot, Value::Uninitialized);
                            old.fast_drop();
                        }
                    } else {
                        let mut scope = scope_ref.borrow_mut();
                        let end = (index as usize + count as usize).min(scope.slots.len());
                        for slot in &mut scope.slots[index as usize..end] {
                            let old = std::mem::replace(slot, Value::Uninitialized);
                            old.fast_drop();
                        }
                    }
                }
                Ok(None)
            }
            Op::JumpIfNullishChain(target) => {
                if matches!(
                    self.vm_stack.last(),
                    Some(Value::Undefined | Value::Null) | None
                ) {
                    let frame = unsafe { self.frames.get_unchecked(top) };
                    let stack_base = frame.stack_base;
                    let chain_base = frame.chain_base as usize;
                    let mark = if self.vm_chains.len() > chain_base {
                        stack_base + self.vm_chains.pop().expect("checked above")
                    } else {
                        self.vm_stack.len()
                    };
                    truncate_stack(&mut self.vm_stack, mark);
                    self.vm_stack.push(Value::Undefined);
                    unsafe { self.frames.get_unchecked_mut(top) }.pc = target as usize;
                }
                Ok(None)
            }
            Op::EndFinally => {
                let pending_base = unsafe { self.frames.get_unchecked(top) }.pending_base as usize;
                let pending = if self.vm_pending.len() > pending_base {
                    self.vm_pending.pop().expect("checked above")
                } else {
                    Pending::Normal
                };
                match pending {
                    Pending::Normal => Ok(None),
                    Pending::Throw(value) => Err(self.throw_value(value)),
                    Pending::Return(value) => self.do_return(frame_floor, value),
                    Pending::Jump {
                        target,
                        handlers,
                        scopes,
                    } => {
                        self.do_exit(target, handlers, scopes);
                        Ok(None)
                    }
                }
            }
            Op::Exit {
                target,
                handlers,
                scopes,
            } => {
                self.do_exit(target, handlers, scopes);
                let target_pc = unsafe { self.frames.get_unchecked(top) }.pc;
                if target_pc <= op_pc {
                    self.steps += 1;
                    if self.steps >= *safepoint_at {
                        *safepoint_at = self.hit_safepoint(sampling, target_pc)?;
                    }
                }
                Ok(None)
            }
            other => self.step_op(other, frame_floor),
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
        let name = self.frames[top].code.names[name as usize].clone();
        self.update_reg_slow_named(top, reg, &name, increment, prefix)
    }

    fn update_reg_slow_named(
        &mut self,
        top: usize,
        reg: u16,
        name: &str,
        increment: bool,
        prefix: bool,
    ) -> Result<(), ScriptError> {
        let slot_index = self.frames[top].reg_base + reg as usize;
        let current = self.vm_stack[slot_index].clone();
        if matches!(current, Value::Uninitialized) {
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
        let name = self.frames[top].code.names[name as usize].clone();
        self.update_local_slow_named(top, depth, index, &name, increment, prefix)
    }

    fn update_local_slow_named(
        &mut self,
        top: usize,
        depth: u16,
        index: u16,
        name: &str,
        increment: bool,
        prefix: bool,
    ) -> Result<(), ScriptError> {
        let current = self.read_local_named(top, depth, index, name)?;
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
        self.write_local_named(top, depth, index, name, new.clone())?;
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
        self.set_prop_slow_with(object, value, name, ic, true)
    }

    fn set_prop_slow_with(
        &mut self,
        object: Value,
        value: Value,
        name: u32,
        ic: u32,
        push_result: bool,
    ) -> Result<(), ScriptError> {
        let top = self.frames.len() - 1;
        let code = self.frames[top].code.clone();
        let cache = &code.ics[ic as usize];
        let key = code.names[name as usize].clone();
        let strict = code.strict;
        let from_shape = match &object {
            Value::Object(handle) => handle.try_borrow().ok().and_then(|o| o.shape_handle()),
            _ => None,
        };
        let keep = if push_result {
            let saved = value.clone();
            self.set_property(&object, &key, value, strict)?;
            Some(saved)
        } else {
            self.set_property(&object, &key, value, strict)?;
            None
        };
        if let Value::Object(handle) = &object
            && let Ok(target) = handle.try_borrow()
        {
            target.record_set_ic(&key, from_shape, cache);
        }
        if let Some(saved) = keep {
            self.vm_stack.push(saved);
        }
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
            Op::ResolveBinding { name, depth, index } => {
                let key = self.frames[top].code.names[name as usize].clone();
                let scope = self.frames[top].scope.clone();
                let (binding_ref, value) = match self.shadowing_loc(scope, &key, depth)? {
                    Some(ShadowLoc::With(object)) => {
                        let value = self.get_property(&object, &key)?;
                        (object, value)
                    }
                    Some(ShadowLoc::Grown(level, slot)) => {
                        let value = Self::with_scope_at(&self.frames[top], level, |scope| {
                            scope.slots[slot as usize].clone()
                        });
                        if matches!(value, Value::Uninitialized) {
                            return Err(self.reference_error(format!(
                                "cannot access {key} before initialization"
                            )));
                        }
                        let packed = (((slot as u32) << 16) | (level as u32)) as f64;
                        (
                            Value::Binding(Rc::new(RefCell::new(Value::Number(packed)))),
                            value,
                        )
                    }
                    None => {
                        let value = self.read_fallback(top, name, depth, index)?;
                        if matches!(value, Value::Uninitialized) {
                            return Err(self.reference_error(format!(
                                "cannot access {key} before initialization"
                            )));
                        }
                        (Value::Undefined, value)
                    }
                };
                self.vm_stack.push(binding_ref);
                self.vm_stack.push(value);
            }
            Op::PutBinding {
                name,
                depth,
                index,
                kind,
            } => {
                let key = self.frames[top].code.names[name as usize].clone();
                let value = self.pop();
                let binding_ref = self.pop();
                let strict = self.frames[top].code.strict;
                match binding_ref {
                    Value::Undefined => {
                        if depth == GLOBAL {
                            self.set_global(&key, value.clone(), strict)?;
                        } else {
                            match kind {
                                BindingKind::Const | BindingKind::Import => {
                                    return Err(self.type_error(format!(
                                        "Assignment to constant variable '{key}'"
                                    )));
                                }
                                BindingKind::SelfName if strict => {
                                    return Err(self.type_error(format!(
                                        "Assignment to constant variable '{key}'"
                                    )));
                                }
                                BindingKind::SelfName => {}
                                _ => self.write_local(top, depth, index, name, value.clone())?,
                            }
                        }
                    }
                    Value::Binding(cell) => {
                        let packed = match *cell.borrow() {
                            Value::Number(n) => n as u32,
                            _ => 0,
                        };
                        let level = (packed & 0xffff) as u16;
                        let slot = (packed >> 16) as usize;
                        let scope = Self::scope_at(&self.frames[top], level);
                        scope.borrow_mut().slots[slot] = value.clone();
                    }
                    object => {
                        if strict && !self.has_property(&object, &key)? {
                            return Err(self.reference_error(format!("{key} is not defined")));
                        }
                        self.set_property(&object, &key, value.clone(), strict)?;
                    }
                }
                self.vm_stack.push(value);
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
                frame.scope = Some(scope);
                frame.scopes += 1;
            }
            Op::PushVarScope(block) => {
                let layout = self.frames[top].code.blocks[block as usize].clone();
                let parent = self.frames[top].scope.clone();
                // a function's variable environment, whose bindings start undefined
                let scope = self.new_scope(layout, parent);
                let frame = &mut self.frames[top];
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
                let frame = &mut self.frames[top];
                let depth = frame.scopes;
                let current = frame.scope.take().expect("a scope to copy");
                let (layout, parent, slots, extra) = {
                    let scope = current.borrow();
                    (
                        scope.layout.clone(),
                        scope.parent.clone(),
                        scope.slots.clone(),
                        scope.extra.clone(),
                    )
                };
                if depth > 0 && depth >= frame.escaped_scopes {
                    super::heap::free_scope(current);
                }
                if frame.escaped_scopes > depth {
                    frame.escaped_scopes = depth;
                }
                let copy = Scope::new_with_slots(layout, parent, slots);
                copy.borrow_mut().extra = extra;
                frame.scope = Some(copy);
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
                let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
                if !self.call_plain(heap, count as usize, method) {
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
                    self.frames[top].mark_scopes_escaped();
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
                self.frames[top].mark_scopes_escaped();
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
            Op::ToIndexKey => {
                let len = self.vm_stack.len();
                if len >= 2
                    && matches!(
                        self.vm_stack[len - 2],
                        Value::Undefined | Value::Null | Value::Uninitialized
                    )
                {
                    return Err(self.type_error("cannot read properties of null or undefined"));
                }
                let keep = match self.vm_stack.last() {
                    Some(Value::String(_)) => true,
                    Some(Value::Number(number)) => {
                        *number >= 0.0 && number.fract() == 0.0 && *number < 4_294_967_295.0
                    }
                    _ => false,
                };
                if !keep {
                    let value = self.pop();
                    let key = self.coerce_key(value)?;
                    self.vm_stack.push(Value::String(key.into()));
                }
            }
            Op::ToPropertyKey => {
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
                if count == 1 && matches!(self.vm_stack.last(), Some(Value::String(_))) {
                    return Ok(None);
                }
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
                self.vm_chains.push(offset);
            }
            Op::JumpIfNullishChain(target) => {
                if matches!(
                    self.vm_stack.last(),
                    Some(Value::Undefined | Value::Null) | None
                ) {
                    let stack_base = self.frames[top].stack_base;
                    let chain_base = self.frames[top].chain_base as usize;
                    let mark = if self.vm_chains.len() > chain_base {
                        stack_base + self.vm_chains.pop().expect("checked above")
                    } else {
                        self.vm_stack.len()
                    };
                    truncate_stack(&mut self.vm_stack, mark);
                    self.vm_stack.push(Value::Undefined);
                    self.frames[top].pc = target as usize;
                }
            }
            Op::ChainEnd => {
                let chain_base = self.frames[top].chain_base as usize;
                if self.vm_chains.len() > chain_base {
                    self.vm_chains.pop();
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
                let iter_slot = self.vm_iters.len() as u32;
                self.vm_iters.push((array, 0));
                self.vm_stack.push(Value::ArrayIter(iter_slot));
            }
            Op::GetAsyncIterator => {
                let value = self.pop();
                let iterator = self.get_async_iterator(value)?;
                self.vm_stack.push(iterator);
            }
            Op::IterValue => {
                if let Some(&Value::ArrayIter(iter_slot)) = self.vm_stack.last() {
                    let (array, index) = &mut self.vm_iters[iter_slot as usize];
                    let value = if *index == u32::MAX {
                        Value::Undefined
                    } else {
                        let idx = *index as usize;
                        match array.borrow().get(idx).cloned() {
                            Some(val) => {
                                *index = (idx + 1) as u32;
                                val.filled()
                            }
                            None => {
                                *index = u32::MAX;
                                Value::Undefined
                            }
                        }
                    };
                    self.vm_stack.push(value);
                } else {
                    let iterator = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                    let value = self.iterator_step(&iterator)?.unwrap_or(Value::Undefined);
                    self.vm_stack.push(value);
                }
            }
            Op::IterRest => {
                if let Some(&Value::ArrayIter(iter_slot)) = self.vm_stack.last() {
                    let (array, index) = &mut self.vm_iters[iter_slot as usize];
                    let rest = if *index == u32::MAX {
                        Vec::new()
                    } else {
                        let idx = *index as usize;
                        *index = u32::MAX;
                        let borrowed = array.borrow();
                        if idx < borrowed.len() {
                            borrowed[idx..].iter().map(|v| v.clone().filled()).collect()
                        } else {
                            Vec::new()
                        }
                    };
                    self.vm_stack.push(Self::new_array(rest));
                } else {
                    let iterator = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                    let mut rest = Vec::new();
                    while let Some(value) = self.iterator_step(&iterator)? {
                        rest.push(value);
                    }
                    self.vm_stack.push(Self::new_array(rest));
                }
            }
            Op::IterClose => {
                let iterator = self.pop();
                if let Value::ArrayIter(iter_slot) = iterator {
                    let slot = iter_slot as usize;
                    if slot < self.vm_iters.len() {
                        self.vm_iters.truncate(slot);
                    }
                } else {
                    self.iterator_close(&iterator)?;
                }
            }
            Op::IterStep(target) => {
                if let Some(&Value::ArrayIter(iter_slot)) = self.vm_stack.last() {
                    let (array, index) = &mut self.vm_iters[iter_slot as usize];
                    let next = if *index == u32::MAX {
                        None
                    } else {
                        let idx = *index as usize;
                        match array.borrow().get(idx).cloned() {
                            Some(val) => {
                                *index = (idx + 1) as u32;
                                Some(val.filled())
                            }
                            None => {
                                *index = u32::MAX;
                                None
                            }
                        }
                    };
                    match next {
                        Some(value) => self.vm_stack.push(value),
                        None => self.frames[top].pc = target as usize,
                    }
                } else {
                    let iterator = self.vm_stack.last().cloned().unwrap_or(Value::Undefined);
                    match self.iterator_step(&iterator)? {
                        Some(value) => self.vm_stack.push(value),
                        None => self.frames[top].pc = target as usize,
                    }
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
                let chains = self.vm_chains.len() - self.frames[top].chain_base as usize;
                let iters = self.vm_iters.len() - self.frames[top].iter_base as usize;
                self.vm_handlers.push(Handler {
                    catch,
                    finally,
                    stack: stack_offset,
                    scopes,
                    chains,
                    iters,
                });
            }
            Op::PopHandler => {
                let base = self.frames[top].handler_base as usize;
                if self.vm_handlers.len() > base {
                    self.vm_handlers.pop();
                }
            }
            Op::CompletionNormal => {
                self.vm_pending.push(Pending::Normal);
            }
            Op::EndFinally => {
                let base = self.frames[top].pending_base as usize;
                let pending = if self.vm_pending.len() > base {
                    self.vm_pending.pop().expect("checked above")
                } else {
                    Pending::Normal
                };
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
                return self.suspend_frame(frame_floor, value);
            }
            Op::SuspendGenerator => {
                return self.suspend_frame(frame_floor, Value::Uninitialized);
            }
            Op::DelegateCall => {
                let sent = self.pop();
                if let Some(Value::ArrayIter(iter_slot)) = self.vm_stack.last() {
                    let iter_slot = *iter_slot;
                    let (array, index) = self.vm_iters[iter_slot as usize].clone();
                    let state = if index == u32::MAX {
                        VmIterator::Done
                    } else {
                        VmIterator::Array {
                            array,
                            index: index as usize,
                        }
                    };
                    if (iter_slot as usize) < self.vm_iters.len() {
                        self.vm_iters.truncate(iter_slot as usize);
                    }
                    *self.vm_stack.last_mut().expect("checked above") =
                        Value::VmIterator(Rc::new(RefCell::new(state)));
                }
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
                if let Some(Value::ArrayIter(iter_slot)) = self.vm_stack.last() {
                    let iter_slot = *iter_slot;
                    let (array, index) = self.vm_iters[iter_slot as usize].clone();
                    let state = if index == u32::MAX {
                        VmIterator::Done
                    } else {
                        VmIterator::Array {
                            array,
                            index: index as usize,
                        }
                    };
                    if (iter_slot as usize) < self.vm_iters.len() {
                        self.vm_iters.truncate(iter_slot as usize);
                    }
                    *self.vm_stack.last_mut().expect("checked above") =
                        Value::VmIterator(Rc::new(RefCell::new(state)));
                }
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
                let mut saved = self.vm_stack.split_off(frame.stack_base);
                truncate_stack(&mut self.vm_stack, frame.return_base);
                for value in &mut saved {
                    if let Value::ArrayIter(iter_slot) = *value {
                        let (array, index) = self.vm_iters[iter_slot as usize].clone();
                        let state = if index == u32::MAX {
                            VmIterator::Done
                        } else {
                            VmIterator::Array {
                                array,
                                index: index as usize,
                            }
                        };
                        *value = Value::VmIterator(Rc::new(RefCell::new(state)));
                    }
                }
                self.vm_iters.truncate(frame.iter_base as usize);
                let handlers = self.vm_handlers.split_off(frame.handler_base as usize);
                let chains = self.vm_chains.split_off(frame.chain_base as usize);
                let pending = self.vm_pending.split_off(frame.pending_base as usize);
                let extra = frame.extra_mut();
                extra.saved_stack = saved;
                extra.handlers = handlers;
                extra.chains = chains;
                extra.pending = pending;
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
            Op::GetRegProp { reg, name, ic } => {
                let slot_index = self.frames[top].reg_base + reg as usize;
                let recv = self.vm_stack[slot_index].clone();
                if matches!(recv, Value::Uninitialized) {
                    let key = self.frames[top].code.layout.names[reg as usize].clone();
                    return Err(
                        self.reference_error(format!("cannot access {key} before initialization"))
                    );
                }
                self.vm_stack.push(recv);
                self.get_prop_slow(name, ic)?;
            }
            Op::GetLocalProp {
                depth,
                index,
                name,
                ic,
            } => {
                let key = self.local_layout_name(top, depth, index);
                let recv = self.read_local_named(top, depth, index, &key)?;
                self.vm_stack.push(recv);
                self.get_prop_slow(name, ic)?;
            }
            Op::GetRegMethod { reg, name, ic } => {
                let slot_index = self.frames[top].reg_base + reg as usize;
                let recv = self.vm_stack[slot_index].clone();
                if matches!(recv, Value::Uninitialized) {
                    let key = self.frames[top].code.layout.names[reg as usize].clone();
                    return Err(
                        self.reference_error(format!("cannot access {key} before initialization"))
                    );
                }
                self.vm_stack.push(recv.clone());
                self.vm_stack.push(recv);
                self.get_prop_slow(name, ic)?;
            }
            Op::GetLocalMethod {
                depth,
                index,
                name,
                ic,
            } => {
                let key = self.local_layout_name(top, depth, index);
                let recv = self.read_local_named(top, depth, index, &key)?;
                self.vm_stack.push(recv.clone());
                self.vm_stack.push(recv);
                self.get_prop_slow(name, ic)?;
            }
            Op::SetRegPop { reg } => {
                let slot_index = self.frames[top].reg_base + reg as usize;
                if matches!(self.vm_stack[slot_index], Value::Uninitialized) {
                    let key = self.frames[top].code.layout.names[reg as usize].clone();
                    return Err(
                        self.reference_error(format!("cannot access {key} before initialization"))
                    );
                }
                let value = self.pop();
                let old = std::mem::replace(&mut self.vm_stack[slot_index], value);
                old.fast_drop();
            }
            Op::SetLocalPop { depth, index } => {
                let value = self.pop();
                let key = self.local_layout_name(top, depth, index);
                self.write_local_named(top, depth, index, &key, value)?;
            }
            Op::SetPropPop { name, ic } => {
                let value = self.pop();
                let object = self.pop();
                self.set_prop_slow_with(object, value, name, ic, false)?;
            }
            Op::SetIndexPop => {
                let value = self.pop();
                let key = self.pop();
                let object = self.pop();
                let strict = self.frames[top].code.strict;
                self.set_index(&object, key, value, strict)?;
            }
            Op::UpdateRegPop { reg, increment } => {
                let key = self.frames[top].code.layout.names[reg as usize].clone();
                self.update_reg_slow_named(top, reg, &key, increment, false)?;
                self.pop().fast_drop();
            }
            Op::UpdateLocalPop {
                depth,
                index,
                increment,
            } => {
                let key = self.local_layout_name(top, depth, index);
                self.update_local_slow_named(top, depth, index, &key, increment, false)?;
                self.pop().fast_drop();
            }
            Op::JumpIfRegFalse { reg, target } | Op::JumpIfRegTrue { reg, target } => {
                let slot_index = self.frames[top].reg_base + reg as usize;
                let value = self.vm_stack[slot_index].clone();
                if matches!(value, Value::Uninitialized) {
                    let key = self.frames[top].code.layout.names[reg as usize].clone();
                    return Err(
                        self.reference_error(format!("cannot access {key} before initialization"))
                    );
                }
                let test = value.truthy();
                value.fast_drop();
                if test == matches!(op, Op::JumpIfRegTrue { .. }) {
                    self.frames[top].pc = target as usize;
                }
            }
            Op::JumpIfNotCompare {
                op: operator,
                target,
            } => {
                let right = self.pop();
                let left = self.pop();
                let passed = self.binary(operator, left, right)?.truthy();
                if !passed {
                    self.frames[top].pc = target as usize;
                }
            }
            Op::JumpIfNotCompareRegs {
                left,
                right,
                op: operator,
                target,
            } => {
                let base = self.frames[top].reg_base;
                let l = self.vm_stack[base + left as usize].clone();
                if matches!(l, Value::Uninitialized) {
                    let key = self.frames[top].code.layout.names[left as usize].clone();
                    return Err(
                        self.reference_error(format!("cannot access {key} before initialization"))
                    );
                }
                let r = self.vm_stack[base + right as usize].clone();
                if matches!(r, Value::Uninitialized) {
                    let key = self.frames[top].code.layout.names[right as usize].clone();
                    return Err(
                        self.reference_error(format!("cannot access {key} before initialization"))
                    );
                }
                let passed = self.binary(operator, l, r)?.truthy();
                if !passed {
                    self.frames[top].pc = target as usize;
                }
            }
            Op::BinaryRegs {
                left,
                right,
                op: operator,
            } => {
                let base = self.frames[top].reg_base;
                let l = self.vm_stack[base + left as usize].clone();
                if matches!(l, Value::Uninitialized) {
                    let key = self.frames[top].code.layout.names[left as usize].clone();
                    return Err(
                        self.reference_error(format!("cannot access {key} before initialization"))
                    );
                }
                let r = self.vm_stack[base + right as usize].clone();
                if matches!(r, Value::Uninitialized) {
                    let key = self.frames[top].code.layout.names[right as usize].clone();
                    return Err(
                        self.reference_error(format!("cannot access {key} before initialization"))
                    );
                }
                let value = self.binary(operator, l, r)?;
                self.vm_stack.push(value);
            }
            Op::GetRegIndex { array, index } => {
                let base = self.frames[top].reg_base;
                let object = self.vm_stack[base + array as usize].clone();
                if matches!(object, Value::Uninitialized) {
                    let key = self.frames[top].code.layout.names[array as usize].clone();
                    return Err(
                        self.reference_error(format!("cannot access {key} before initialization"))
                    );
                }
                let key = self.vm_stack[base + index as usize].clone();
                if matches!(key, Value::Uninitialized) {
                    let name = self.frames[top].code.layout.names[index as usize].clone();
                    return Err(
                        self.reference_error(format!("cannot access {name} before initialization"))
                    );
                }
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
            Op::SplitRegexPop {
                pattern,
                flags,
                cache,
            } => {
                let heap = unsafe { &*super::heap::heap_ptr(self.heap_id) };
                let code = self.frames[top].code.clone();
                let handled = if let Some(slot) = self.vm_stack.last_mut() {
                    let slot_ptr: *mut Value = slot;
                    // safety: try_split_regex_pop only reads self.intrinsics and mutates *slot_ptr
                    self.try_split_regex_pop(heap, &code, pattern, flags, cache, unsafe {
                        &mut *slot_ptr
                    })
                } else {
                    false
                };
                if !handled {
                    let recv = self.pop();
                    let split_fn = self.get_property(&recv, "split")?;
                    let pat = code.names[pattern as usize].clone();
                    let flg = code.names[flags as usize].clone();
                    let re = self.new_regexp(&pat, &flg)?;
                    let arr = self.call_function(split_fn, recv, vec![re])?;
                    let pop_fn = self.get_property(&arr, "pop")?;
                    let res = self.call_function(pop_fn, arr, Vec::new())?;
                    self.vm_stack.push(res);
                }
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

    fn suspend_frame(
        &mut self,
        frame_floor: usize,
        value: Value,
    ) -> Result<Option<RunResult>, ScriptError> {
        if self.frames.len() != frame_floor + 1 {
            return Err(runtime_error("yield outside a resumable frame"));
        }
        let mut frame = self.frames.pop().expect("generator frame");
        let mut saved = self.vm_stack.split_off(frame.stack_base);
        truncate_stack(&mut self.vm_stack, frame.return_base);
        // materialize inline array iterators before suspending so vm_iters stays LIFO
        for value in &mut saved {
            if let Value::ArrayIter(iter_slot) = *value {
                let (array, index) = self.vm_iters[iter_slot as usize].clone();
                let state = if index == u32::MAX {
                    VmIterator::Done
                } else {
                    VmIterator::Array {
                        array,
                        index: index as usize,
                    }
                };
                *value = Value::VmIterator(Rc::new(RefCell::new(state)));
            }
        }
        self.vm_iters.truncate(frame.iter_base as usize);
        let handlers = self.vm_handlers.split_off(frame.handler_base as usize);
        let chains = self.vm_chains.split_off(frame.chain_base as usize);
        let pending = self.vm_pending.split_off(frame.pending_base as usize);
        let extra = frame.extra_mut();
        extra.saved_stack = saved;
        extra.handlers = handlers;
        extra.chains = chains;
        extra.pending = pending;
        Ok(Some(RunResult::Yield(value, Box::new(frame))))
    }

    fn make_generator(
        &mut self,
        function: &Value,
        closure: &Rc<Closure>,
        frame: Frame,
    ) -> Result<Value, ScriptError> {
        let default_proto = if closure.code.is_async {
            self.intrinsics.async_generator_prototype.clone()
        } else {
            self.intrinsics.generator_prototype.clone()
        };
        let generator = if closure.code.is_async {
            let data = Rc::new(RefCell::new(AsyncGeneratorData {
                state: GeneratorState::SuspendedStart(Box::new(frame)),
                queue: VecDeque::new(),
                awaiting: false,
            }));
            new_object(JsObject::with_prototype(
                Some(default_proto.clone()),
                ObjectClass::AsyncGenerator(data),
            ))
        } else {
            let state = Rc::new(RefCell::new(GeneratorState::SuspendedStart(Box::new(
                frame,
            ))));
            new_object(JsObject::with_prototype(
                Some(default_proto.clone()),
                ObjectClass::Generator(state),
            ))
        };
        // root the generator while reading function.prototype so a getter cannot collect the frame
        let root_base = self.temp_roots.len();
        self.temp_roots.push(Value::Object(generator.clone()));
        let proto_val = self.get_property(function, "prototype");
        self.temp_roots.truncate(root_base);
        let prototype = match proto_val? {
            Value::Object(prototype) => Some(prototype),
            Value::Array(array) => Some(self.array_prototype_wrapper(&array)),
            _ => None,
        };
        if let Some(prototype) = prototype {
            generator.borrow_mut().prototype = Some(prototype);
        }
        Ok(Value::Object(generator))
    }

    // runs a generator to its next yield; the result is an iterator result object
    pub(super) fn generator_resume(
        &mut self,
        state: &Rc<RefCell<GeneratorState>>,
        mode: ResumeMode,
        value: Value,
    ) -> Result<Value, ScriptError> {
        let current = std::mem::replace(&mut *state.borrow_mut(), GeneratorState::Executing);
        let (mut frame, started) = match current {
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
            GeneratorState::SuspendedStart(frame) => (frame, false),
            GeneratorState::Suspended(frame) => (frame, true),
        };
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
        let fulfilled = Value::builtin(Builtin::native(Native::AsyncResume(
            AsyncResumeHandle(state.clone()),
            false,
        )));
        let rejected = Value::builtin(Builtin::native(Native::AsyncResume(
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
            GeneratorState::SuspendedStart(_)
                | GeneratorState::Suspended(_)
                | GeneratorState::Completed
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
            let (frame, started) = match state {
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
                GeneratorState::SuspendedStart(frame) => (frame, false),
                GeneratorState::Suspended(frame) => (frame, true),
            };
            data.borrow_mut().queue.push_front(AsyncGeneratorRequest {
                mode: request.mode,
                value: Value::Undefined,
                promise: request.promise.clone(),
            });
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
            return_target: std::ptr::null(),
            handler_base: 0,
            chain_base: 0,
            pending_base: 0,
            iter_base: 0,
            scope,
            this,
            function,
            scopes: 0,
            escaped_scopes: 0,
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

enum ShadowLoc {
    With(Value),
    Grown(u16, u16),
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

#[cfg(test)]
#[test]
fn parameter_tdz_sync_generators_compound_assign_and_restricted_props() {
    let document =
        crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
    let document = Rc::new(RefCell::new(document));
    let mut runtime = ScriptRuntime::new(document.clone());
    runtime
        .execute(
            r#"
            const seen = [];
            try { (function (a = b, b = 1) {})(); } catch (e) { seen.push(e.name); }
            seen.push((function (a, b = arguments[0]) { return b; })(7));
            let genBodyRan = false;
            function* badGen(a = (() => { throw new RangeError("sync"); })()) { genBodyRan = true; yield a; }
            try { badGen(); } catch (e) { seen.push(e.name, genBodyRan); }
            function* okGen(a = 5) { genBodyRan = true; yield a; }
            const g1 = okGen();
            seen.push(genBodyRan, g1.return(9).value, genBodyRan);
            const g2 = okGen();
            seen.push(g2.next().value, genBodyRan);
            try { (function (p = eval("var arguments")) {})(); } catch (e) { seen.push(e.name); }
            seen.push(((p = eval("var arguments = 'ok'")) => arguments)());
            let keyRead = false;
            const key = { toString() { keyRead = true; return "k"; } };
            try { null[key] += 1; } catch (e) { seen.push(e.name, keyRead); }
            const obj = { x: 10 };
            with (obj) { x += (delete obj.x, 5); }
            seen.push(obj.x);
            const del = { get x() { delete this.x; return 2; } };
            with (del) {
                (function () {
                    "use strict";
                    try { x += 1; seen.push("ok"); } catch (e) { seen.push(e.name); }
                })();
            }
            const cd = Object.getOwnPropertyDescriptor(Function.prototype, "caller");
            const ad = Object.getOwnPropertyDescriptor(Function.prototype, "arguments");
            seen.push(cd.get === cd.set, cd.get === ad.get);
            function sloppy() { return sloppy.caller; }
            seen.push(sloppy());
            try { (function () { "use strict"; }).caller; } catch (e) { seen.push(e.name); }
            document.getElementById("out").textContent = seen.join("|");
            "#,
        )
        .expect("script should run");
    let out = document.borrow().find_element_by_id("out").unwrap();
    assert_eq!(
        document.borrow().element_text_content(out).as_deref(),
        Some(
            "ReferenceError|7|RangeError|false|false|9|false|5|true|SyntaxError|ok|TypeError|false|15|ReferenceError|true|true||TypeError"
        )
    );
}
