// lowers the syntax tree to bytecode for the stack machine in vm.rs; names resolve at
// compile time to (depth, slot) pairs on a chain of heap scopes, and anything that does not
// resolve falls back to the global environment by name

use super::ScriptError;
use super::ast::*;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Op {
    Nop,
    Undefined,
    Null,
    True,
    False,
    Number(f64),
    BigInt(u32),
    String(u32),
    Regex {
        pattern: u32,
        flags: u32,
    },
    TemplateObject(u32),
    Pop,
    Dup,
    Dup2,
    Dup3,
    Swap,
    // [a b c] -> [c a b]
    RotateDown3,
    // [a b c d] -> [d a b c]
    RotateDown4,
    // [a b c] -> [b c a]
    RotateUp3,
    // [a b c d] -> [b c d a]
    RotateUp4,
    // [a b c d e] -> [e a b c d]
    RotateDown5,
    // throws a TypeError when the value on top is null or undefined, leaving it there
    RequireCoercible,
    // [source excluded-keys] -> [copy of the remaining own enumerable properties]
    CopyRest,
    // the home object of the running method, for super lookups
    Home,
    GetLocal {
        depth: u16,
        index: u16,
        name: u32,
    },
    SetLocal {
        depth: u16,
        index: u16,
        name: u32,
    },
    InitLocal {
        depth: u16,
        index: u16,
    },
    GetGlobal(u32),
    SetGlobal(u32),
    InitGlobal(u32),
    // a name no scope knew at compile time: a sloppy direct eval may have added it
    // to the chain, so the chain is looked over before the global
    GetLookup(u32),
    SetLookup(u32),
    // removes a binding a direct eval grew past a scope's compiled layout, which
    // starts as deletable; the plain slots the compiler laid out never are
    DeleteGrown {
        depth: u16,
        index: u16,
    },
    // a top-level const of a classic script: a global binding that rejects assignment
    InitGlobalConst(u32),
    DeclareGlobal(u32),
    TypeofGlobal(u32),
    DeleteGlobal(u32),
    // a block scope with the layout at this index of the code's blocks
    PushScope(u16),
    // a function's variable environment, which parameter expressions move into a
    // scope of its own over the frame's
    PushVarScope(u16),
    PopScope,
    Debugger,
    CloneScope,
    This,
    GlobalThis,
    NewTarget,
    ImportMeta,
    // copies frame state into hidden slots of the function scope so arrows can read it
    InitThisSlot(u16),
    InitArgumentsSlot(u16),
    InitNewTargetSlot(u16),
    InitHomeSlot(u16),
    NewObject,
    NewArray,
    ArrayPush,
    ArrayHole,
    // the not-iterable message naming the spread expression, from the code's names
    ArraySpread(Option<u32>),
    // a spread call argument, with the name its source prints as
    ArgumentSpread(u32),
    DefineField(u32),
    DefineComputed,
    DefineAccessor {
        getter: bool,
        computed: bool,
        name: u32,
    },
    ObjectSpread,
    SetPrototype,
    GetProp(u32),
    GetIndex,
    SetProp(u32),
    SetIndex,
    DeleteProp(u32),
    DeleteIndex,
    // [object] -> [object has]: whether a with object answers for the name
    WithHas(u32),
    GetSuper(u32),
    GetSuperIndex,
    SetSuper(u32),
    SetSuperIndex,
    GetPrivate,
    SetPrivate,
    HasPrivate,
    NewPrivateName(u32),
    Call(u16),
    CallMethod(u16),
    CallSpread,
    CallMethodSpread,
    // a call of the global eval by name, the spec's one direct eval, which runs in
    // the calling frame's strictness
    CallEval(u16),
    New(u16),
    NewSpread,
    SuperCall(u16),
    SuperCallSpread,
    Closure(u32),
    ClosureNamed(u32),
    Class {
        template: u32,
        has_super: bool,
    },
    DefineMethod {
        is_static: bool,
        kind: u8,
        computed: bool,
        name: u32,
    },
    // object literal methods: [object (key) function] -> [object]
    DefineLiteralMethod {
        computed: bool,
        name: u32,
    },
    AddField {
        is_static: bool,
        private: bool,
        computed: bool,
        name: u32,
    },
    RunStaticInit,
    Binary(BinaryOperator),
    Negate,
    Plus,
    Not,
    BitNot,
    TypeOf,
    ToNumeric,
    ToPropertyKey,
    Increment,
    Decrement,
    TemplateConcat(u16),
    Jump(u32),
    JumpIfFalse(u32),
    JumpIfTrue(u32),
    JumpIfFalseKeep(u32),
    JumpIfTrueKeep(u32),
    JumpIfNotNullishKeep(u32),
    JumpIfNotUndefined(u32),
    ChainStart,
    JumpIfNullishChain(u32),
    ChainEnd,
    GetIterator(Option<u32>),
    GetAsyncIterator,
    IterValue,
    IterRest,
    IterClose,
    IterStep(u32),
    AsyncIterNext,
    IterResultUnpack(u32),
    ForInPrepare,
    ForInNext(u32),
    Throw,
    ThrowConstAssign(u32),
    PushHandler {
        catch: u32,
        finally: u32,
    },
    PopHandler,
    CompletionNormal,
    EndFinally,
    Exit {
        target: u32,
        handlers: u16,
        scopes: u16,
    },
    Return,
    Yield,
    YieldDelegating,
    DelegateStep(u32),
    // async yield*: [iterator sent] -> [iterator promise] calling next, throw or return
    DelegateCall,
    // async yield*: [iterator result] -> [iterator value], or the final value at the target
    DelegateUnpack(u32),
    Await,
    DynamicImport,
}

pub(super) const NO_TARGET: u32 = u32::MAX;

// method kinds carried by DefineMethod
pub(super) const METHOD: u8 = 0;
pub(super) const GETTER: u8 = 1;
pub(super) const SETTER: u8 = 2;
pub(super) const PRIVATE_METHOD: u8 = 3;
pub(super) const PRIVATE_GETTER: u8 = 4;
pub(super) const PRIVATE_SETTER: u8 = 5;

#[derive(Debug)]
pub(super) struct Code {
    pub(super) ops: Vec<Op>,
    pub(super) names: Vec<Name>,
    pub(super) children: Vec<Rc<Code>>,
    pub(super) classes: Vec<Rc<ClassTemplate>>,
    pub(super) templates: Vec<TemplateSite>,
    // the names of the function scope's slots, and of each block scope's
    pub(super) layout: Rc<[Name]>,
    pub(super) blocks: Vec<Rc<[Name]>>,
    pub(super) param_slots: Vec<Option<u16>>,
    pub(super) kind: FunctionKind,
    pub(super) is_async: bool,
    pub(super) is_generator: bool,
    pub(super) strict: bool,
    pub(super) name: Name,
    pub(super) length: u32,
    pub(super) source: Option<(Rc<str>, usize, usize)>,
    // the whole source this was compiled from, and where in it each statement's ops start, as
    // (op index, byte offset) pairs in op order
    pub(super) text: Rc<str>,
    pub(super) positions: Box<[(u32, u32)]>,
    // how many debugger breakpoints sit in these ops
    pub(super) breakpoints: Cell<u32>,
    // a named function expression sees its own name in an enclosing one-slot scope
    pub(super) self_scope: Option<Rc<[Name]>>,
    // simple parameter lists bind straight into slots; others run destructuring code
    pub(super) simple_params: bool,
}

#[derive(Debug)]
pub(super) struct TemplateSite {
    pub(super) cooked: Vec<Option<Name>>,
    pub(super) raw: Vec<Name>,
}

#[derive(Debug)]
pub(super) struct ClassTemplate {
    pub(super) constructor: Rc<Code>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BindingKind {
    Var,
    Let,
    Const,
    Function,
    Param,
    Class,
    Hidden,
    Import,
}

#[derive(Clone, Copy, Debug)]
struct Binding {
    index: u16,
    kind: BindingKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    Function,
    Block,
    // a scope that exists at runtime in the parent chain but belongs to no function
    Outer,
}

struct ScopeInfo {
    kind: ScopeKind,
    bindings: HashMap<Name, Binding>,
    size: u16,
    materialized: bool,
    // the with object's hidden slot
    with_object: Option<Name>,
    // a materialized block's index in the function's blocks
    block: u16,
    // bindings at or past this index were grown at run time by a direct eval, so
    // they are deletable; u16::MAX when none are
    grown_from: u16,
}

impl ScopeInfo {
    fn new(kind: ScopeKind, materialized: bool) -> Self {
        Self {
            kind,
            bindings: HashMap::new(),
            size: 0,
            materialized,
            with_object: None,
            block: 0,
            grown_from: u16::MAX,
        }
    }

    // the scope's names in slot order
    fn layout(&self) -> Rc<[Name]> {
        let mut names = vec![Name::from(""); self.size as usize];
        for (name, binding) in &self.bindings {
            names[binding.index as usize] = name.clone();
        }
        names.into()
    }

    fn declare(&mut self, name: Name, kind: BindingKind) -> u16 {
        if let Some(existing) = self.bindings.get(&name) {
            return existing.index;
        }
        let index = self.size;
        self.size += 1;
        self.bindings.insert(name, Binding { index, kind });
        index
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    Loop,
    Switch,
    Block,
}

struct LoopTarget {
    labels: Vec<Name>,
    kind: TargetKind,
    breaks: Vec<usize>,
    continues: Vec<usize>,
    handlers: u16,
    scopes: u16,
    // scopes counted where a continue lands, inside a per-iteration scope
    continue_scopes: u16,
}

struct FunctionState {
    ops: Vec<Op>,
    names: Vec<Name>,
    name_index: HashMap<Name, u32>,
    children: Vec<Rc<Code>>,
    classes: Vec<Rc<ClassTemplate>>,
    templates: Vec<TemplateSite>,
    scopes: Vec<ScopeInfo>,
    blocks: Vec<Rc<[Name]>>,
    positions: Vec<(u32, u32)>,
    is_arrow: bool,
    // global script code: top-level declarations live in the global environment
    is_global: bool,
    targets: Vec<LoopTarget>,
    handler_depth: u16,
    // scopes pushed at runtime since the function scope, for break and continue
    runtime_scopes: u16,
    this_slot: Option<u16>,
    arguments_slot: Option<u16>,
    new_target_slot: Option<u16>,
    home_slot: Option<u16>,
    temps: u16,
    // the slot console and eval scripts keep their statements' completion value in
    completion: Option<Name>,
    // body vars declare here when parameter expressions move them out of the
    // function scope, as the function instantiation steps give
    var_target: Option<usize>,
    strict: bool,
    pending_labels: Vec<Name>,
    // async generators await yielded and returned values
    async_generator: bool,
}

impl FunctionState {
    fn new(is_arrow: bool, is_global: bool, strict: bool) -> Self {
        Self {
            ops: Vec::new(),
            names: Vec::new(),
            name_index: HashMap::new(),
            children: Vec::new(),
            classes: Vec::new(),
            templates: Vec::new(),
            scopes: Vec::new(),
            blocks: Vec::new(),
            positions: Vec::new(),
            is_arrow,
            is_global,
            targets: Vec::new(),
            handler_depth: 0,
            runtime_scopes: 0,
            this_slot: None,
            arguments_slot: None,
            new_target_slot: None,
            home_slot: None,
            temps: 0,
            completion: None,
            var_target: None,
            strict,
            pending_labels: Vec::new(),
            async_generator: false,
        }
    }

    fn name(&mut self, name: &str) -> u32 {
        if let Some(index) = self.name_index.get(name) {
            return *index;
        }
        let index = self.names.len() as u32;
        let name: Name = name.into();
        self.names.push(name.clone());
        self.name_index.insert(name, index);
        index
    }

    fn emit(&mut self, op: Op) -> usize {
        self.ops.push(op);
        self.ops.len() - 1
    }

    fn here(&self) -> u32 {
        self.ops.len() as u32
    }

    fn patch(&mut self, at: usize, target: u32) {
        self.ops[at] = match self.ops[at] {
            Op::Jump(_) => Op::Jump(target),
            Op::JumpIfFalse(_) => Op::JumpIfFalse(target),
            Op::JumpIfTrue(_) => Op::JumpIfTrue(target),
            Op::JumpIfFalseKeep(_) => Op::JumpIfFalseKeep(target),
            Op::JumpIfTrueKeep(_) => Op::JumpIfTrueKeep(target),
            Op::JumpIfNotNullishKeep(_) => Op::JumpIfNotNullishKeep(target),
            Op::JumpIfNotUndefined(_) => Op::JumpIfNotUndefined(target),
            Op::JumpIfNullishChain(_) => Op::JumpIfNullishChain(target),
            Op::IterStep(_) => Op::IterStep(target),
            Op::IterResultUnpack(_) => Op::IterResultUnpack(target),
            Op::ForInNext(_) => Op::ForInNext(target),
            Op::DelegateStep(_) => Op::DelegateStep(target),
            Op::DelegateUnpack(_) => Op::DelegateUnpack(target),
            Op::Exit {
                handlers, scopes, ..
            } => Op::Exit {
                target,
                handlers,
                scopes,
            },
            Op::PushHandler { catch, finally } => {
                if catch == NO_TARGET - 1 {
                    Op::PushHandler {
                        catch: target,
                        finally,
                    }
                } else {
                    Op::PushHandler {
                        catch,
                        finally: target,
                    }
                }
            }
            other => other,
        };
    }

    fn function_scope_index(&self) -> usize {
        self.scopes
            .iter()
            .position(|scope| scope.kind == ScopeKind::Function)
            .unwrap_or(0)
    }
}

pub(super) struct Compiler {
    functions: Vec<FunctionState>,
    source: Rc<str>,
    // short-circuit jumps of the optional chain being compiled
    chain_jumps: Vec<usize>,
    // the not-iterable message for the pattern about to be compiled, from its initializer
    pattern_callsite: Option<u32>,
}

pub(super) struct ModuleCode {
    pub(super) code: Rc<Code>,
    pub(super) imports: Vec<(Name, Vec<(ImportBinding, u16)>)>,
    pub(super) exports: Vec<(Name, u16)>,
    pub(super) reexports: Vec<(Name, Name, Name)>,
    pub(super) star_exports: Vec<Name>,
    pub(super) namespace_exports: Vec<(Name, Name)>,
}

// one of a direct eval caller's live scopes, innermost first
pub(super) struct CallerScope {
    // the scope's names, with any a direct eval added past the layout
    pub(super) names: Vec<Name>,
    pub(super) slots: u16,
    pub(super) var_scope: bool,
}

enum Folded {
    Number(f64),
    Bool(bool),
    String(String),
}

// the literal v8's parser folds an operation on literals into, which its errors then print
fn folded(expression: &Expr) -> Option<Folded> {
    use super::operations::{to_int32_number as int32, to_uint32_number as uint32};
    let number = |expression: &Expr| match folded(expression) {
        Some(Folded::Number(value)) => Some(value),
        _ => None,
    };
    Some(match expression {
        Expr::Number(value) => Folded::Number(*value),
        Expr::Bool(value) => Folded::Bool(*value),
        Expr::String(value) => Folded::String(value.to_string()),
        Expr::Unary {
            operator: UnaryOperator::Not,
            argument,
        } => Folded::Bool(match &**argument {
            Expr::Null => true,
            _ => match folded(argument)? {
                Folded::Number(value) => value == 0.0 || value.is_nan(),
                Folded::Bool(value) => !value,
                Folded::String(value) => value.is_empty(),
            },
        }),
        Expr::Unary { operator, argument } => {
            let value = number(argument)?;
            Folded::Number(match operator {
                UnaryOperator::Negative => -value,
                UnaryOperator::Positive => value,
                UnaryOperator::BitNot => f64::from(!int32(value)),
                _ => return None,
            })
        }
        Expr::Binary {
            operator,
            left,
            right,
        } => match (operator, folded(left)?, folded(right)?) {
            (BinaryOperator::Add, Folded::String(left), Folded::String(right)) => {
                Folded::String(left + &right)
            }
            (operator, Folded::Number(left), Folded::Number(right)) => {
                Folded::Number(match operator {
                    BinaryOperator::Add => left + right,
                    BinaryOperator::Subtract => left - right,
                    BinaryOperator::Multiply => left * right,
                    BinaryOperator::Divide => left / right,
                    BinaryOperator::Remainder => left % right,
                    BinaryOperator::BitAnd => f64::from(int32(left) & int32(right)),
                    BinaryOperator::BitOr => f64::from(int32(left) | int32(right)),
                    BinaryOperator::BitXor => f64::from(int32(left) ^ int32(right)),
                    BinaryOperator::ShiftLeft => f64::from(int32(left) << (uint32(right) & 31)),
                    BinaryOperator::ShiftRight => f64::from(int32(left) >> (uint32(right) & 31)),
                    BinaryOperator::ShiftRightUnsigned => {
                        f64::from(uint32(left) >> (uint32(right) & 31))
                    }
                    _ => return None,
                })
            }
            _ => return None,
        },
        _ => return None,
    })
}

// an expression as v8's call printer shows it in an error (src/ast/prettyprinter.cc)
fn callsite(expression: &Expr) -> String {
    const INTERMEDIATE: &str = "(intermediate value)";
    match folded(expression) {
        Some(Folded::Number(value)) => return super::number_to_string(value),
        Some(Folded::Bool(value)) => return value.to_string(),
        Some(Folded::String(value)) => return format!("\"{value}\""),
        None => {}
    }
    match expression {
        Expr::Identifier(name) => name.to_string(),
        Expr::This => "this".to_string(),
        Expr::Null => "null".to_string(),
        Expr::Template {
            quasis,
            expressions,
        } if expressions.is_empty() => format!("\"{}\"", quasis.concat()),
        Expr::Member {
            object, property, ..
        } => format!("{}.{property}", callsite(object)),
        Expr::Index { object, index, .. } => match &**index {
            Expr::String(key) => format!("{}.{key}", callsite(object)),
            key => format!("{}[{}]", callsite(object), callsite(key)),
        },
        Expr::Call { callee, .. } => callsite(callee),
        Expr::Array(elements) => {
            let elements = elements.iter().map(|element| match element {
                ArrayElement::Hole => String::new(),
                ArrayElement::Item(item) => callsite(item),
                ArrayElement::Spread(item) => format!("(...{})", callsite(item)),
            });
            format!("[{}]", elements.collect::<Vec<_>>().join(","))
        }
        Expr::Object(properties) => format!("{{{}}}", INTERMEDIATE.repeat(properties.len())),
        Expr::Unary { operator, argument } => {
            let token = match operator {
                UnaryOperator::Not => "!",
                UnaryOperator::Negative => "-",
                UnaryOperator::Positive => "+",
                UnaryOperator::BitNot => "~",
                UnaryOperator::TypeOf => "typeof ",
                UnaryOperator::Void => "void ",
                UnaryOperator::Delete => "delete ",
            };
            format!("({token}{})", callsite(argument))
        }
        Expr::Binary {
            operator,
            left,
            right,
        } => {
            let token = match operator {
                BinaryOperator::Add => "+",
                BinaryOperator::Subtract => "-",
                BinaryOperator::Multiply => "*",
                BinaryOperator::Divide => "/",
                BinaryOperator::Remainder => "%",
                BinaryOperator::Exponent => "**",
                BinaryOperator::ShiftLeft => "<<",
                BinaryOperator::ShiftRight => ">>",
                BinaryOperator::ShiftRightUnsigned => ">>>",
                BinaryOperator::BitAnd => "&",
                BinaryOperator::BitOr => "|",
                BinaryOperator::BitXor => "^",
                BinaryOperator::Equal => "==",
                BinaryOperator::NotEqual => "!=",
                BinaryOperator::StrictEqual => "===",
                BinaryOperator::StrictNotEqual => "!==",
                BinaryOperator::Less => "<",
                BinaryOperator::LessEqual => "<=",
                BinaryOperator::Greater => ">",
                BinaryOperator::GreaterEqual => ">=",
                BinaryOperator::InstanceOf => "instanceof",
                BinaryOperator::In => "in",
            };
            format!("({} {token} {})", callsite(left), callsite(right))
        }
        Expr::Logical {
            operator,
            left,
            right,
        } => {
            let token = match operator {
                LogicalOperator::And => "&&",
                LogicalOperator::Or => "||",
                LogicalOperator::Nullish => "??",
            };
            format!("({} {token} {})", callsite(left), callsite(right))
        }
        Expr::Sequence(expressions) => {
            let expressions = expressions.iter().map(callsite).collect::<Vec<_>>();
            format!("({})", expressions.join(" , "))
        }
        Expr::Conditional { .. } => INTERMEDIATE.repeat(3),
        Expr::Assign { target, .. } => match &**target {
            Pattern::Identifier(name) => name.to_string(),
            Pattern::Expression(target) => callsite(target),
            _ => INTERMEDIATE.to_string(),
        },
        _ => INTERMEDIATE.to_string(),
    }
}

fn error(message: impl Into<String>) -> ScriptError {
    ScriptError {
        offset: 0,
        message: message.into(),
        name: "SyntaxError".to_string(),
    }
}

// lexical declarations made directly in a statement list
fn lexical_declarations(statements: &[Stmt]) -> Vec<(Name, BindingKind)> {
    let mut output = Vec::new();
    for statement in statements {
        match statement {
            Stmt::Declaration { kind, declarators } if *kind != DeclarationKind::Var => {
                let binding = if *kind == DeclarationKind::Const {
                    BindingKind::Const
                } else {
                    BindingKind::Let
                };
                let mut names = Vec::new();
                for declarator in declarators {
                    bound_names(&declarator.target, &mut names);
                }
                output.extend(names.into_iter().map(|name| (name, binding)));
            }
            Stmt::Class(class) => {
                if let Some(name) = &class.name {
                    output.push((name.clone(), BindingKind::Class));
                }
            }
            _ => {}
        }
    }
    output
}

pub(super) fn block_functions(statements: &[Stmt]) -> Vec<Rc<FunctionNode>> {
    statements
        .iter()
        .filter_map(|statement| match statement {
            Stmt::Function(function) => Some(function.clone()),
            _ => None,
        })
        .collect()
}

fn property_name(key: &PropertyKey) -> Option<&str> {
    match key {
        PropertyKey::Static(name) => Some(name),
        _ => None,
    }
}

impl Compiler {
    pub(super) fn new(source: &str) -> Self {
        Self {
            functions: Vec::new(),
            source: source.into(),
            chain_jumps: Vec::new(),
            pattern_callsite: None,
        }
    }

    fn state(&mut self) -> &mut FunctionState {
        self.functions
            .last_mut()
            .expect("a function is being compiled")
    }

    fn emit(&mut self, op: Op) -> usize {
        self.state().emit(op)
    }

    fn here(&mut self) -> u32 {
        self.state().here()
    }

    fn patch_here(&mut self, at: usize) {
        let here = self.here();
        self.state().patch(at, here);
    }

    fn name(&mut self, name: &str) -> u32 {
        self.state().name(name)
    }

    // v8's call printer names the iterated expression, and a call by its callee with both ways
    // the call can fail
    fn iterable_callsite(&mut self, expression: &Expr) -> u32 {
        let message = match expression {
            Expr::Call { callee, .. } => format!(
                "{} is not a function or its return value is not iterable",
                callsite(callee)
            ),
            other => format!("{} is not iterable", callsite(other)),
        };
        self.name(&message)
    }

    pub(super) fn compile_script(self, script: &Script) -> Result<Rc<Code>, ScriptError> {
        self.compile_global(script, false, false)
    }

    // a script that returns its last statement's value when that statement is an expression,
    // as a console shows it
    pub(super) fn compile_console_script(self, script: &Script) -> Result<Rc<Code>, ScriptError> {
        self.compile_global(script, true, false)
    }

    // eval code: its lexical declarations live in a scope of its own, instantiated
    // uninitialized before the body runs, and its vars and functions too when it is
    // strict; a sloppy eval still declares its vars on the caller's global
    pub(super) fn compile_eval_script(self, script: &Script) -> Result<Rc<Code>, ScriptError> {
        self.compile_global(script, true, true)
    }

    // a direct eval inside a function: the caller's live scopes sit under the eval's
    // own, innermost first, so its code resolves the caller's bindings through them;
    // a sloppy one declares its vars and functions in the caller's variable
    // environment, growing it by the bindings that are new, whose names are returned
    // for the caller to add
    pub(super) fn compile_eval_in_scope(
        mut self,
        script: &Script,
        caller: &[CallerScope],
    ) -> Result<(Rc<Code>, Vec<Name>), ScriptError> {
        let mut state = FunctionState::new(false, false, script.strict);
        // the caller's scopes, outermost first so resolution walks them under the eval's own
        for scope in caller.iter().rev() {
            let mut info = ScopeInfo::new(ScopeKind::Block, true);
            info.size = scope.slots;
            // bindings past the live slots are ones earlier evals grew, deletable
            info.grown_from = scope.slots;
            for (index, name) in scope.names.iter().enumerate() {
                info.bindings.insert(
                    name.clone(),
                    Binding {
                        index: index as u16,
                        kind: BindingKind::Var,
                    },
                );
            }
            state.scopes.push(info);
        }
        // the variable environment among them, the innermost one a sloppy eval extends
        let var_target = caller
            .iter()
            .position(|scope| scope.var_scope)
            .map(|position| caller.len() - 1 - position);
        self.functions.push(state);
        let mut vars = Vec::new();
        collect_var_names(&script.body, &mut vars);
        let functions = block_functions(&script.body);
        let lexicals = lexical_declarations(&script.body);
        let mut grown: Vec<Name> = Vec::new();
        let scoped_declarations =
            if script.strict {
                let mut declarations = lexicals;
                declarations.extend(vars.iter().map(|name| (name.clone(), BindingKind::Var)));
                declarations.extend(functions.iter().map(|function| {
                    (
                        function.name.clone().expect("declarations are named"),
                        BindingKind::Var,
                    )
                }));
                declarations
            } else {
                match var_target {
                    Some(target) => {
                        // each name the caller's scopes do not know becomes a binding in
                        // its variable environment, appended past the live slots
                        for name in vars.iter().chain(functions.iter().map(|function| {
                            function.name.as_ref().expect("declarations are named")
                        })) {
                            if self.resolve(name).is_none() {
                                grown.push(name.clone());
                                self.state().scopes[target].declare(name.clone(), BindingKind::Var);
                            }
                        }
                    }
                    None => {
                        for name in &vars {
                            let index = self.name(name);
                            self.emit(Op::DeclareGlobal(index));
                        }
                    }
                }
                lexicals
            };
        // the eval's own scope, holding its temporaries and, when strict, its vars
        self.state()
            .scopes
            .push(ScopeInfo::new(ScopeKind::Function, true));
        let own = self.state().scopes.len() - 1;
        let eval_scope = !scoped_declarations.is_empty();
        if eval_scope {
            self.push_block_scope(&scoped_declarations);
        }
        if script.strict {
            // the pushed scope holds the vars uninitialized, so they take undefined
            // before the body runs
            for name in &vars {
                self.emit(Op::Undefined);
                self.compile_set_name(name, true)?;
                self.emit(Op::Pop);
            }
            for function in &functions {
                self.compile_function_value(function)?;
                let name = function.name.clone().expect("declarations are named");
                self.compile_set_name(&name, true)?;
                self.emit(Op::Pop);
            }
        } else {
            for function in &functions {
                self.compile_function_value(function)?;
                let name = function.name.clone().expect("declarations are named");
                self.compile_set_name(&name, true)?;
                self.emit(Op::Pop);
            }
        }
        let completion = self.alloc_temp();
        self.state().completion = Some(completion.clone());
        self.clear_completion()?;
        for statement in &script.body {
            self.compile_statement(statement)?;
        }
        if eval_scope {
            self.pop_block_scope();
        }
        self.compile_get_name(&completion)?;
        self.emit(Op::Return);
        let state = self.functions.pop().expect("eval state");
        let layout = state.scopes[own].layout();
        let mut code = self.finish(
            state,
            None,
            FunctionKind::Normal,
            false,
            false,
            "",
            0,
            false,
            true,
            Vec::new(),
        );
        code.layout = layout;
        Ok((Rc::new(code), grown))
    }
    fn compile_global(
        mut self,
        script: &Script,
        completion: bool,
        eval_code: bool,
    ) -> Result<Rc<Code>, ScriptError> {
        self.functions
            .push(FunctionState::new(false, true, script.strict));
        // global declarations: var and function names become global bindings up front
        let mut vars = Vec::new();
        collect_var_names(&script.body, &mut vars);
        let functions = block_functions(&script.body);
        let lexicals = lexical_declarations(&script.body);
        // a strict eval keeps its vars and functions in its own scope; a sloppy one's
        // become globals, since its caller's var environment is the global
        let scoped_declarations = if eval_code && script.strict {
            let mut declarations = lexicals;
            declarations.extend(vars.iter().map(|name| (name.clone(), BindingKind::Var)));
            declarations.extend(functions.iter().map(|function| {
                (
                    function.name.clone().expect("declarations are named"),
                    BindingKind::Var,
                )
            }));
            declarations
        } else {
            for name in &vars {
                let index = self.name(name);
                self.emit(Op::DeclareGlobal(index));
            }
            lexicals
        };
        // the VM creates this scope for the frame; it holds the script's temporaries
        self.state()
            .scopes
            .push(ScopeInfo::new(ScopeKind::Function, true));
        // the scope the eval's declarations live in, pushed before the body so they
        // exist uninitialized from the first statement
        let eval_scope = eval_code && !scoped_declarations.is_empty();
        if eval_scope {
            self.push_block_scope(&scoped_declarations);
        }
        if eval_code && script.strict {
            for name in &vars {
                self.emit(Op::Undefined);
                self.compile_set_name(name, true)?;
                self.emit(Op::Pop);
            }
            for function in functions {
                self.compile_function_value(&function)?;
                let name = function.name.clone().expect("declarations are named");
                self.compile_set_name(&name, true)?;
                self.emit(Op::Pop);
            }
        } else {
            for function in functions {
                self.compile_function_value(&function)?;
                let name = function.name.clone().expect("declarations are named");
                let index = self.name(&name);
                self.emit(Op::InitGlobal(index));
            }
        }
        if completion {
            let slot = self.alloc_temp();
            self.state().completion = Some(slot);
            self.clear_completion()?;
        }
        for statement in &script.body {
            self.compile_statement(statement)?;
        }
        if eval_scope {
            self.pop_block_scope();
        }
        match self.state().completion.clone() {
            Some(slot) => self.compile_get_name(&slot)?,
            None => {
                self.emit(Op::Undefined);
            }
        }
        self.emit(Op::Return);
        let state = self.functions.pop().expect("script state");
        let layout = state.scopes[0].layout();
        let mut code = self.finish(
            state,
            None,
            FunctionKind::Normal,
            false,
            false,
            "",
            0,
            false,
            true,
            Vec::new(),
        );
        code.layout = layout;
        Ok(Rc::new(code))
    }

    pub(super) fn compile_module(mut self, module: &Module) -> Result<ModuleCode, ScriptError> {
        let mut state = FunctionState::new(false, false, true);
        state.scopes.push(ScopeInfo::new(ScopeKind::Function, true));
        self.functions.push(state);
        for _ in 0..4 {
            self.emit(Op::Nop);
        }
        let mut imports = Vec::new();
        let mut exports = Vec::new();
        let mut reexports = Vec::new();
        let mut star_exports = Vec::new();
        let mut namespace_exports = Vec::new();
        let mut statements = Vec::new();
        for item in &module.items {
            match item {
                ModuleItem::Import(import) => {
                    let mut bindings = Vec::new();
                    for binding in &import.bindings {
                        let local = match binding {
                            ImportBinding::Named { local, .. }
                            | ImportBinding::Namespace(local) => local.clone(),
                        };
                        let slot = self.declare(&local, BindingKind::Import);
                        bindings.push((binding.clone(), slot));
                    }
                    imports.push((import.specifier.clone(), bindings));
                }
                ModuleItem::Statement(statement) => statements.push(statement.clone()),
                ModuleItem::Export(export) => match export {
                    ExportDeclaration::Declaration(statement) => {
                        let mut names = Vec::new();
                        match statement {
                            Stmt::Declaration { declarators, .. } => {
                                for declarator in declarators {
                                    bound_names(&declarator.target, &mut names);
                                }
                            }
                            Stmt::Function(function) => names.extend(function.name.clone()),
                            Stmt::Class(class) => names.extend(class.name.clone()),
                            _ => {}
                        }
                        for name in names {
                            exports.push((name.clone(), name));
                        }
                        statements.push(statement.clone());
                    }
                    ExportDeclaration::Default { local, statement } => {
                        exports.push(("default".into(), local.clone()));
                        statements.push(statement.clone());
                    }
                    ExportDeclaration::Named { bindings, from } => match from {
                        Some(from) => {
                            for (imported, exported) in bindings {
                                reexports.push((from.clone(), imported.clone(), exported.clone()));
                            }
                        }
                        None => {
                            for (local, exported) in bindings {
                                exports.push((exported.clone(), local.clone()));
                            }
                        }
                    },
                    ExportDeclaration::All { from, alias } => match alias {
                        Some(alias) => namespace_exports.push((from.clone(), alias.clone())),
                        None => star_exports.push(from.clone()),
                    },
                },
            }
        }
        self.declare_function_body(&statements)?;
        let mut export_slots = Vec::new();
        for (exported, local) in exports {
            let slot = match self.state().scopes[0].bindings.get(&local) {
                Some(binding) => binding.index,
                None => return Err(error(format!("module exports undeclared {local}"))),
            };
            export_slots.push((exported, slot));
        }
        for statement in &statements {
            self.compile_statement(statement)?;
        }
        self.emit(Op::Undefined);
        self.emit(Op::Return);
        let state = self.functions.pop().expect("module state");
        let code = Rc::new(self.finish(
            state,
            None,
            FunctionKind::Normal,
            true,
            false,
            "",
            0,
            false,
            true,
            Vec::new(),
        ));
        Ok(ModuleCode {
            code,
            imports,
            exports: export_slots,
            reexports,
            star_exports,
            namespace_exports,
        })
    }

    pub(super) fn compile_function(
        mut self,
        function: &FunctionNode,
    ) -> Result<Rc<Code>, ScriptError> {
        // a Function constructor body sees only the global scope
        self.functions.push(FunctionState::new(false, true, false));
        self.state()
            .scopes
            .push(ScopeInfo::new(ScopeKind::Block, false));
        let code = self.build_function(function)?;
        self.functions.pop();
        Ok(code)
    }

    // a function that sees the names of a paused frame's scopes, given outermost first in slot
    // order, as a closure over that frame's innermost scope does
    pub(super) fn compile_function_within(
        mut self,
        function: &FunctionNode,
        layers: &[Rc<[Name]>],
    ) -> Result<Rc<Code>, ScriptError> {
        self.functions.push(FunctionState::new(false, true, false));
        for names in layers {
            let mut scope = ScopeInfo::new(ScopeKind::Outer, true);
            for (slot, name) in names.iter().enumerate() {
                let name = match name.is_empty() {
                    true => Name::from(format!("%slot{slot}").as_str()),
                    false => name.clone(),
                };
                scope.declare(name, BindingKind::Let);
            }
            self.state().scopes.push(scope);
        }
        let code = self.build_function(function)?;
        self.functions.pop();
        Ok(code)
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        state: FunctionState,
        source: Option<(usize, usize)>,
        kind: FunctionKind,
        is_async: bool,
        is_generator: bool,
        name: &str,
        length: u32,
        self_named: bool,
        simple_params: bool,
        param_slots: Vec<Option<u16>>,
    ) -> Code {
        let layout = match state.is_global {
            true => Rc::from([]),
            false => state
                .scopes
                .iter()
                .find(|scope| scope.kind == ScopeKind::Function)
                .map_or_else(|| Rc::from([]), ScopeInfo::layout),
        };
        let mut ops = state.ops;
        // the prologue reserves four slots that only become real when an arrow needed them
        if ops.len() >= 4 && !state.is_global {
            if let Some(slot) = state.this_slot {
                ops[0] = Op::InitThisSlot(slot);
            }
            if let Some(slot) = state.arguments_slot {
                ops[1] = Op::InitArgumentsSlot(slot);
            }
            if let Some(slot) = state.new_target_slot {
                ops[2] = Op::InitNewTargetSlot(slot);
            }
            if let Some(slot) = state.home_slot {
                ops[3] = Op::InitHomeSlot(slot);
            }
        }
        Code {
            ops,
            names: state.names,
            children: state.children,
            classes: state.classes,
            templates: state.templates,
            layout,
            blocks: state.blocks,
            param_slots,
            kind,
            is_async,
            is_generator,
            strict: state.strict,
            name: name.into(),
            length,
            source: source.map(|(start, end)| (self.source.clone(), start, end)),
            text: self.source.clone(),
            positions: state.positions.into(),
            breakpoints: Cell::new(0),
            self_scope: self_named.then(|| Rc::from([Name::from(name)])),
            simple_params,
        }
    }

    // ---- scopes and names ----

    fn declare(&mut self, name: &Name, kind: BindingKind) -> u16 {
        let state = self.state();
        let scope = state.scopes.last_mut().expect("a scope is open");
        scope.declare(name.clone(), kind)
    }

    fn declare_in_function_scope(&mut self, name: &Name, kind: BindingKind) -> u16 {
        let state = self.state();
        let index = state.function_scope_index();
        state.scopes[index].declare(name.clone(), kind)
    }

    // the body's variable environment when parameter expressions give it a scope
    // of its own: materialized above the parameters, so the bindings the body
    // declares land over them and closures from the parameter list never see them
    fn push_var_environment(&mut self) {
        let mut scope = ScopeInfo::new(ScopeKind::Function, true);
        let state = self.state();
        scope.block = state.blocks.len() as u16;
        let layout = scope.layout();
        state.blocks.push(layout);
        state.emit(Op::PushVarScope(scope.block));
        state.runtime_scopes += 1;
        state.scopes.push(scope);
        state.var_target = Some(state.scopes.len() - 1);
    }

    fn push_block_scope(&mut self, declarations: &[(Name, BindingKind)]) -> bool {
        let materialized = !declarations.is_empty();
        let mut scope = ScopeInfo::new(ScopeKind::Block, materialized);
        for (name, kind) in declarations {
            scope.declare(name.clone(), *kind);
        }
        if materialized {
            let state = self.state();
            scope.block = state.blocks.len() as u16;
            state.blocks.push(scope.layout());
            state.emit(Op::PushScope(scope.block));
            state.runtime_scopes += 1;
        }
        self.state().scopes.push(scope);
        materialized
    }

    fn pop_block_scope(&mut self) {
        let state = self.state();
        let scope = state.scopes.pop().expect("a block scope is open");
        if scope.materialized {
            // a block scope can grow after it was opened, as temporaries are added
            state.blocks[scope.block as usize] = scope.layout();
            state.emit(Op::PopScope);
            state.runtime_scopes -= 1;
        }
    }

    // finds a name from the innermost scope outward, across enclosing functions
    fn resolve(&mut self, name: &str) -> Option<(u16, u16, BindingKind)> {
        let mut depth = 0u16;
        for function_index in (0..self.functions.len()).rev() {
            let state = &self.functions[function_index];
            for scope in state.scopes.iter().rev() {
                if let Some(binding) = scope.bindings.get(name) {
                    return Some((depth, binding.index, binding.kind));
                }
                if scope.materialized {
                    depth += 1;
                }
            }
            if state.is_global {
                return None;
            }
        }
        None
    }

    // whether a resolved binding sits past its scope's compiled layout, where a
    // direct eval grew it at run time
    fn grown_binding(&self, depth: u16, index: u16) -> bool {
        let mut up = depth;
        for state in self.functions.iter().rev() {
            for scope in state.scopes.iter().rev() {
                if up == 0 {
                    return index >= scope.grown_from;
                }
                if scope.materialized {
                    up -= 1;
                }
            }
        }
        false
    }

    // the with objects a name passes on its way out to where it is bound, innermost first
    fn with_objects(&self, name: &str) -> Vec<Name> {
        let mut objects = Vec::new();
        // the compiler's own temporaries are never properties of anything
        if name.starts_with('%') {
            return objects;
        }
        for state in self.functions.iter().rev() {
            for scope in state.scopes.iter().rev() {
                if scope.bindings.contains_key(name) {
                    return objects;
                }
                if let Some(object) = &scope.with_object {
                    objects.push(object.clone());
                }
            }
            if state.is_global {
                break;
            }
        }
        objects
    }

    // for each with object around a name: when the object has the name, emit `found` with
    // the object on the stack and skip to the end; otherwise fall through without it
    fn compile_with_lookups(
        &mut self,
        name: &str,
        objects: &[Name],
        mut found: impl FnMut(&mut Self, u32),
    ) -> Result<Vec<usize>, ScriptError> {
        let index = self.name(name);
        let mut ends = Vec::new();
        for object in objects {
            self.compile_get_name_direct(object)?;
            self.emit(Op::WithHas(index));
            let missing = self.emit(Op::JumpIfFalse(0));
            found(self, index);
            ends.push(self.emit(Op::Jump(0)));
            self.patch_here(missing);
            self.emit(Op::Pop);
        }
        Ok(ends)
    }

    fn compile_get_name(&mut self, name: &Name) -> Result<(), ScriptError> {
        let objects = self.with_objects(name);
        if objects.is_empty() {
            return self.compile_get_name_direct(name);
        }
        let ends = self.compile_with_lookups(name, &objects, |compiler, index| {
            compiler.emit(Op::GetProp(index));
        })?;
        self.compile_get_name_direct(name)?;
        for end in ends {
            self.patch_here(end);
        }
        Ok(())
    }

    fn compile_get_name_direct(&mut self, name: &Name) -> Result<(), ScriptError> {
        if &**name == "undefined" && self.resolve(name).is_none() {
            self.emit(Op::Undefined);
            return Ok(());
        }
        if &**name == "arguments"
            && self.resolve(name).is_none()
            && let Some((depth, index)) = self.lexical_slot(Lexical::Arguments)
        {
            let name_index = self.name(name);
            self.emit(Op::GetLocal {
                depth,
                index,
                name: name_index,
            });
            return Ok(());
        }
        let name_index = self.name(name);
        match self.resolve(name) {
            Some((depth, index, _)) => {
                self.emit(Op::GetLocal {
                    depth,
                    index,
                    name: name_index,
                });
            }
            None => {
                // a name no scope knows may be one a sloppy direct eval added to
                // the chain at run time, so code in a sloppy function looks there
                if !self.state().strict && !self.state().is_global {
                    self.emit(Op::GetLookup(name_index));
                } else {
                    self.emit(Op::GetGlobal(name_index));
                }
            }
        }
        Ok(())
    }

    // stores the value on top of the stack, leaving it there
    fn compile_set_name(&mut self, name: &Name, initialize: bool) -> Result<(), ScriptError> {
        let objects = if initialize {
            Vec::new()
        } else {
            self.with_objects(name)
        };
        if objects.is_empty() {
            return self.compile_set_name_direct(name, initialize);
        }
        let ends = self.compile_with_lookups(name, &objects, |compiler, index| {
            compiler.emit(Op::Swap);
            compiler.emit(Op::SetProp(index));
        })?;
        self.compile_set_name_direct(name, initialize)?;
        for end in ends {
            self.patch_here(end);
        }
        Ok(())
    }

    fn compile_set_name_direct(
        &mut self,
        name: &Name,
        initialize: bool,
    ) -> Result<(), ScriptError> {
        let name_index = self.name(name);
        match self.resolve(name) {
            Some((depth, index, kind)) => {
                if initialize {
                    self.emit(Op::Dup);
                    self.emit(Op::InitLocal { depth, index });
                } else if matches!(kind, BindingKind::Const | BindingKind::Import) {
                    self.emit(Op::ThrowConstAssign(name_index));
                } else {
                    self.emit(Op::SetLocal {
                        depth,
                        index,
                        name: name_index,
                    });
                }
            }
            None => {
                if initialize {
                    self.emit(Op::InitGlobal(name_index));
                } else if !self.state().strict && !self.state().is_global {
                    self.emit(Op::SetLookup(name_index));
                } else {
                    self.emit(Op::SetGlobal(name_index));
                }
            }
        }
        Ok(())
    }

    fn clear_completion(&mut self) -> Result<(), ScriptError> {
        if let Some(slot) = self.state().completion.clone() {
            self.emit(Op::Undefined);
            self.compile_set_name(&slot, true)?;
            self.emit(Op::Pop);
        }
        Ok(())
    }

    fn alloc_temp(&mut self) -> Name {
        let state = self.state();
        state.temps += 1;
        let name: Name = format!("%temp{}", state.temps).into();
        self.declare_in_function_scope(&name, BindingKind::Hidden);
        name
    }

    // ---- hidden lexical state for arrow functions ----

    fn lexical_slot(&mut self, which: Lexical) -> Option<(u16, u16)> {
        // the innermost non-arrow function owns this, arguments, new.target and home
        let owner = (0..self.functions.len())
            .rev()
            .find(|index| !self.functions[*index].is_arrow)?;
        if self.functions[owner].is_global {
            return None;
        }
        let name: Name = match which {
            Lexical::This => "%this",
            Lexical::Arguments => "%arguments",
            Lexical::NewTarget => "%newtarget",
            Lexical::Home => "%home",
        }
        .into();
        let state = &mut self.functions[owner];
        let scope_index = state.function_scope_index();
        let slot = state.scopes[scope_index].declare(name.clone(), BindingKind::Hidden);
        match which {
            Lexical::This => state.this_slot = Some(slot),
            Lexical::Arguments => state.arguments_slot = Some(slot),
            Lexical::NewTarget => state.new_target_slot = Some(slot),
            Lexical::Home => state.home_slot = Some(slot),
        }
        self.resolve(&name).map(|(depth, index, _)| (depth, index))
    }

    fn in_arrow(&self) -> bool {
        self.functions.last().is_some_and(|state| state.is_arrow)
    }

    fn compile_lexical(&mut self, which: Lexical, direct: Op) {
        if self.in_arrow()
            && let Some((depth, index)) = self.lexical_slot(which)
        {
            let name = self.name("this");
            self.emit(Op::GetLocal { depth, index, name });
            return;
        }
        let top_level_arrow = self.in_arrow();
        if top_level_arrow && which == Lexical::This {
            self.emit(Op::GlobalThis);
            return;
        }
        self.emit(direct);
    }

    // ---- statements ----

    fn declare_function_body(&mut self, body: &[Stmt]) -> Result<(), ScriptError> {
        let mut vars = Vec::new();
        collect_var_names(body, &mut vars);
        for name in vars {
            // body vars sit in the function's variable environment, which parameter
            // expressions move into a scope of its own
            let target = self
                .state()
                .var_target
                .unwrap_or_else(|| self.state().function_scope_index());
            self.state().scopes[target].declare(name, BindingKind::Var);
        }
        for (name, kind) in lexical_declarations(body) {
            self.declare(&name, kind);
        }
        let functions = block_functions(body);
        for function in &functions {
            let name = function.name.clone().expect("declarations are named");
            self.declare(&name, BindingKind::Function);
        }
        for function in functions {
            self.compile_function_value(&function)?;
            let name = function.name.clone().expect("declarations are named");
            self.compile_set_name(&name, true)?;
            self.emit(Op::Pop);
        }
        Ok(())
    }

    fn compile_statements(&mut self, statements: &[Stmt]) -> Result<(), ScriptError> {
        for statement in statements {
            self.compile_statement(statement)?;
        }
        Ok(())
    }

    fn compile_block(&mut self, body: &[Stmt]) -> Result<(), ScriptError> {
        let mut declarations = lexical_declarations(body);
        let functions = block_functions(body);
        for function in &functions {
            declarations.push((
                function.name.clone().expect("declarations are named"),
                BindingKind::Function,
            ));
        }
        self.push_block_scope(&declarations);
        for function in functions {
            self.compile_function_value(&function)?;
            let name = function.name.clone().expect("declarations are named");
            self.compile_set_name(&name, true)?;
            // sloppy code also gives the function a var binding in the enclosing function
            if !self.state().strict && self.state().is_global {
                let name_index = self.name(&name);
                self.emit(Op::Dup);
                self.emit(Op::SetGlobal(name_index));
                self.emit(Op::Pop);
            } else if !self.state().strict
                && self
                    .state()
                    .scopes
                    .iter()
                    .any(|scope| scope.kind == ScopeKind::Function)
            {
                let function_scope = self
                    .state()
                    .var_target
                    .unwrap_or_else(|| self.state().function_scope_index());
                if !self.state().scopes[function_scope]
                    .bindings
                    .contains_key(&name)
                {
                    self.state().scopes[function_scope].declare(name.clone(), BindingKind::Var);
                }
                let index = self.state().scopes[function_scope].bindings[&name].index;
                let depth = self.state().scopes[function_scope + 1..]
                    .iter()
                    .filter(|scope| scope.materialized)
                    .count() as u16;
                let name_index = self.name(&name);
                self.emit(Op::Dup);
                self.emit(Op::SetLocal {
                    depth,
                    index,
                    name: name_index,
                });
                self.emit(Op::Pop);
            }
            self.emit(Op::Pop);
        }
        self.compile_statements(body)?;
        self.pop_block_scope();
        Ok(())
    }

    fn compile_statement(&mut self, statement: &Stmt) -> Result<(), ScriptError> {
        if let Stmt::Position(offset) = statement {
            let state = self.state();
            let at = state.here();
            // a statement that left no ops, such as a hoisted declaration, gives its place to
            // the next one
            match state.positions.last_mut() {
                Some(last) if last.0 == at => last.1 = *offset as u32,
                _ => state.positions.push((at, *offset as u32)),
            }
            return Ok(());
        }
        let labels = std::mem::take(&mut self.state().pending_labels);
        // these statements complete with undefined unless a statement inside gives a value
        if matches!(
            statement,
            Stmt::If { .. }
                | Stmt::With { .. }
                | Stmt::While { .. }
                | Stmt::DoWhile { .. }
                | Stmt::For { .. }
                | Stmt::ForIn { .. }
                | Stmt::ForOf { .. }
                | Stmt::Switch { .. }
                | Stmt::Try { .. }
        ) {
            self.clear_completion()?;
        }
        match statement {
            Stmt::Empty | Stmt::Function(_) | Stmt::Position(_) => Ok(()),
            Stmt::Debugger => {
                self.emit(Op::Debugger);
                Ok(())
            }
            Stmt::Expression(expression) => {
                self.compile_expression(expression)?;
                if let Some(slot) = self.state().completion.clone() {
                    self.compile_set_name(&slot, true)?;
                }
                self.emit(Op::Pop);
                Ok(())
            }
            Stmt::Block(body) => {
                if labels.is_empty() {
                    return self.compile_block(body);
                }
                self.push_target(labels, TargetKind::Block);
                self.compile_block(body)?;
                self.pop_target(None);
                Ok(())
            }
            Stmt::Declaration { kind, declarators } => {
                for declarator in declarators {
                    match &declarator.init {
                        Some(init) => {
                            self.compile_named_expression(init, &declarator.target)?;
                            // v8 finds the source of a destructured initializer only when the
                            // error's position is where it starts, as with a name or a literal
                            let starts = matches!(
                                init,
                                Expr::Identifier(_)
                                    | Expr::This
                                    | Expr::Null
                                    | Expr::Array(_)
                                    | Expr::Object(_)
                            ) || folded(init).is_some();
                            self.pattern_callsite = starts.then(|| self.iterable_callsite(init));
                            self.compile_binding(&declarator.target, *kind)?;
                        }
                        None if *kind == DeclarationKind::Let => {
                            self.emit(Op::Undefined);
                            self.compile_binding(&declarator.target, *kind)?;
                        }
                        None => {}
                    }
                }
                Ok(())
            }
            Stmt::Class(class) => {
                self.compile_class(class)?;
                let name = class.name.clone().expect("declarations are named");
                self.compile_set_name(&name, true)?;
                self.emit(Op::Pop);
                Ok(())
            }
            Stmt::If {
                test,
                consequent,
                alternate,
            } => {
                self.compile_expression(test)?;
                let skip = self.emit(Op::JumpIfFalse(0));
                self.compile_statement(consequent)?;
                match alternate {
                    Some(alternate) => {
                        let end = self.emit(Op::Jump(0));
                        self.patch_here(skip);
                        self.compile_statement(alternate)?;
                        self.patch_here(end);
                    }
                    None => self.patch_here(skip),
                }
                Ok(())
            }
            Stmt::With { object, body } => {
                // the object sits in a hidden slot that names inside the body consult first
                let temp = self.alloc_temp();
                self.compile_expression(object)?;
                self.emit(Op::RequireCoercible);
                self.compile_set_name(&temp, true)?;
                self.emit(Op::Pop);
                let mut scope = ScopeInfo::new(ScopeKind::Block, false);
                scope.with_object = Some(temp);
                self.state().scopes.push(scope);
                let compiled = self.compile_statement(body);
                self.state().scopes.pop();
                compiled
            }
            Stmt::While { test, body } => {
                self.push_target(labels, TargetKind::Loop);
                let start = self.here();
                self.compile_expression(test)?;
                let exit = self.emit(Op::JumpIfFalse(0));
                self.compile_statement(body)?;
                self.emit(Op::Jump(start));
                self.patch_here(exit);
                self.pop_target(Some(start));
                Ok(())
            }
            Stmt::DoWhile { body, test } => {
                self.push_target(labels, TargetKind::Loop);
                let start = self.here();
                self.compile_statement(body)?;
                let continue_at = self.here();
                self.compile_expression(test)?;
                self.emit(Op::JumpIfTrue(start));
                self.pop_target(Some(continue_at));
                Ok(())
            }
            Stmt::For {
                init,
                test,
                update,
                body,
            } => self.compile_for(labels, init, test, update, body),
            Stmt::ForIn { left, right, body } => self.compile_for_in(labels, left, right, body),
            Stmt::ForOf {
                left,
                right,
                body,
                is_await,
            } => self.compile_for_of(labels, left, right, body, *is_await),
            Stmt::Switch {
                discriminant,
                cases,
            } => self.compile_switch(labels, discriminant, cases),
            Stmt::Break(label) => self.compile_exit(label.as_ref(), false),
            Stmt::Continue(label) => self.compile_exit(label.as_ref(), true),
            Stmt::Labeled { label, body } => {
                let mut labels = labels;
                labels.push(label.clone());
                match body.as_ref() {
                    Stmt::For { .. }
                    | Stmt::ForIn { .. }
                    | Stmt::ForOf { .. }
                    | Stmt::While { .. }
                    | Stmt::DoWhile { .. }
                    | Stmt::Switch { .. }
                    | Stmt::Labeled { .. } => {
                        self.state().pending_labels = labels;
                        self.compile_statement(body)
                    }
                    _ => {
                        self.push_target(labels, TargetKind::Block);
                        self.compile_statement(body)?;
                        self.pop_target(None);
                        Ok(())
                    }
                }
            }
            Stmt::Return(argument) => {
                match argument {
                    Some(argument) => self.compile_expression(argument)?,
                    None => {
                        self.emit(Op::Undefined);
                    }
                }
                if self.state().async_generator {
                    self.emit(Op::Await);
                }
                self.emit(Op::Return);
                Ok(())
            }
            Stmt::Throw(argument) => {
                self.compile_expression(argument)?;
                self.emit(Op::Throw);
                Ok(())
            }
            Stmt::Try {
                block,
                handler,
                finalizer,
            } => self.compile_try(block, handler.as_ref(), finalizer.as_deref()),
        }
    }

    fn push_target(&mut self, labels: Vec<Name>, kind: TargetKind) {
        let state = self.state();
        let handlers = state.handler_depth;
        let scopes = state.runtime_scopes;
        state.targets.push(LoopTarget {
            labels,
            kind,
            breaks: Vec::new(),
            continues: Vec::new(),
            handlers,
            scopes,
            continue_scopes: scopes,
        });
    }

    fn pop_target(&mut self, continue_at: Option<u32>) {
        let target = self.state().targets.pop().expect("a target is open");
        let here = self.here();
        for at in target.breaks {
            self.state().patch(at, here);
        }
        if let Some(continue_at) = continue_at {
            for at in target.continues {
                self.state().patch(at, continue_at);
            }
        }
    }

    fn compile_exit(&mut self, label: Option<&Name>, is_continue: bool) -> Result<(), ScriptError> {
        let state = self.state();
        let position = state.targets.iter().rposition(|target| match label {
            Some(label) => {
                target.labels.contains(label) && (!is_continue || target.kind == TargetKind::Loop)
            }
            None if is_continue => target.kind == TargetKind::Loop,
            None => target.kind != TargetKind::Block,
        });
        let Some(position) = position else {
            return Err(error(match label {
                Some(label) => format!("undefined label {label}"),
                None if is_continue => "continue outside a loop".to_string(),
                None => "break outside a loop or switch".to_string(),
            }));
        };
        let target = &state.targets[position];
        let handlers = target.handlers;
        let scopes = if is_continue {
            target.continue_scopes
        } else {
            target.scopes
        };
        let at = state.emit(Op::Exit {
            target: 0,
            handlers,
            scopes,
        });
        let target = &mut state.targets[position];
        if is_continue {
            target.continues.push(at);
        } else {
            target.breaks.push(at);
        }
        Ok(())
    }

    fn compile_for(
        &mut self,
        labels: Vec<Name>,
        init: &Option<ForInit>,
        test: &Option<Expr>,
        update: &Option<Expr>,
        body: &Stmt,
    ) -> Result<(), ScriptError> {
        let mut lexical = false;
        match init {
            Some(ForInit::Declaration { kind, declarators }) if *kind != DeclarationKind::Var => {
                let binding = if *kind == DeclarationKind::Const {
                    BindingKind::Const
                } else {
                    BindingKind::Let
                };
                let mut names = Vec::new();
                for declarator in declarators {
                    bound_names(&declarator.target, &mut names);
                }
                let declarations: Vec<_> = names.into_iter().map(|name| (name, binding)).collect();
                lexical = self.push_block_scope(&declarations);
                self.compile_statement(&Stmt::Declaration {
                    kind: *kind,
                    declarators: declarators.clone(),
                })?;
            }
            Some(ForInit::Declaration { kind, declarators }) => {
                self.compile_statement(&Stmt::Declaration {
                    kind: *kind,
                    declarators: declarators.clone(),
                })?;
            }
            Some(ForInit::Expression(expression)) => {
                self.compile_expression(expression)?;
                self.emit(Op::Pop);
            }
            None => {}
        }
        self.push_target(labels, TargetKind::Loop);
        let start = self.here();
        let exit = match test {
            Some(test) => {
                self.compile_expression(test)?;
                Some(self.emit(Op::JumpIfFalse(0)))
            }
            None => None,
        };
        self.compile_statement(body)?;
        let continue_at = self.here();
        // each iteration gets its own copy of the loop bindings, so closures keep their value
        if lexical {
            self.emit(Op::CloneScope);
        }
        if let Some(update) = update {
            self.compile_expression(update)?;
            self.emit(Op::Pop);
        }
        self.emit(Op::Jump(start));
        if let Some(exit) = exit {
            self.patch_here(exit);
        }
        self.pop_target(Some(continue_at));
        if lexical {
            self.pop_block_scope();
        }
        Ok(())
    }

    // binds the value on top of the stack to a for-in or for-of head, consuming it
    fn compile_for_head_binding(&mut self, left: &ForHead) -> Result<bool, ScriptError> {
        match left {
            ForHead::Declaration { kind, target } if *kind != DeclarationKind::Var => {
                let binding = if *kind == DeclarationKind::Const {
                    BindingKind::Const
                } else {
                    BindingKind::Let
                };
                let mut names = Vec::new();
                bound_names(target, &mut names);
                let declarations: Vec<_> = names.into_iter().map(|name| (name, binding)).collect();
                let pushed = self.push_block_scope(&declarations);
                self.compile_binding(target, *kind)?;
                Ok(pushed)
            }
            ForHead::Declaration { kind, target } => {
                self.compile_binding(target, *kind)?;
                Ok(false)
            }
            ForHead::Target(target) => {
                self.compile_assign_pattern(target)?;
                Ok(false)
            }
        }
    }

    fn compile_for_in(
        &mut self,
        labels: Vec<Name>,
        left: &ForHead,
        right: &Expr,
        body: &Stmt,
    ) -> Result<(), ScriptError> {
        self.compile_expression(right)?;
        self.emit(Op::ForInPrepare);
        self.push_target(labels, TargetKind::Loop);
        // the key iterator stays on the stack under every iteration
        let start = self.here();
        let exit = self.emit(Op::ForInNext(0));
        // a continue lands at the loop head, outside the per-iteration scope
        let pushed = self.compile_for_head_binding(left)?;
        self.compile_statement(body)?;
        if pushed {
            self.pop_block_scope();
        }
        self.emit(Op::Jump(start));
        self.patch_here(exit);
        self.close_loop_with_iterator(start, false)
    }

    fn close_loop_with_iterator(
        &mut self,
        continue_at: u32,
        close: bool,
    ) -> Result<(), ScriptError> {
        // breaks land after the iterator is popped; they close it first when it is a real iterator
        let target = self.state().targets.pop().expect("loop target");
        let normal_exit = self.emit(Op::Jump(0));
        let break_landing = self.here();
        if close {
            self.emit(Op::IterClose);
        } else {
            self.emit(Op::Pop);
        }
        let end = self.emit(Op::Jump(0));
        let done_landing = self.here();
        self.state().patch(normal_exit, done_landing);
        self.emit(Op::Pop);
        self.patch_here(end);
        for at in target.breaks {
            self.state().patch(at, break_landing);
        }
        for at in target.continues {
            self.state().patch(at, continue_at);
        }
        Ok(())
    }

    fn compile_for_of(
        &mut self,
        labels: Vec<Name>,
        left: &ForHead,
        right: &Expr,
        body: &Stmt,
        is_await: bool,
    ) -> Result<(), ScriptError> {
        self.compile_expression(right)?;
        let op = if is_await {
            Op::GetAsyncIterator
        } else {
            Op::GetIterator(Some(self.iterable_callsite(right)))
        };
        self.emit(op);
        self.push_target(labels, TargetKind::Loop);
        let start = self.here();
        let exit = if is_await {
            self.emit(Op::AsyncIterNext);
            self.emit(Op::Await);
            self.emit(Op::IterResultUnpack(0))
        } else {
            self.emit(Op::IterStep(0))
        };
        // a throw from the body closes the iterator before it propagates
        let handler = self.emit(Op::PushHandler {
            catch: NO_TARGET - 1,
            finally: NO_TARGET,
        });
        self.state().handler_depth += 1;
        // a continue lands after the per-iteration scope is popped
        let pushed = self.compile_for_head_binding(left)?;
        self.compile_statement(body)?;
        if pushed {
            self.pop_block_scope();
        }
        self.emit(Op::PopHandler);
        self.state().handler_depth -= 1;
        let continue_at = self.here();
        self.emit(Op::Jump(start));
        // catch: [iterator, error] -> close the iterator, then rethrow
        self.patch_here(handler);
        self.emit(Op::Swap);
        self.emit(Op::IterClose);
        self.emit(Op::Throw);
        self.patch_here(exit);
        self.close_loop_with_iterator(continue_at, true)
    }

    fn compile_switch(
        &mut self,
        labels: Vec<Name>,
        discriminant: &Expr,
        cases: &[SwitchCase],
    ) -> Result<(), ScriptError> {
        self.compile_expression(discriminant)?;
        let mut declarations = Vec::new();
        let mut functions = Vec::new();
        for case in cases {
            declarations.extend(lexical_declarations(&case.body));
            functions.extend(block_functions(&case.body));
        }
        for function in &functions {
            declarations.push((function.name.clone().expect("named"), BindingKind::Function));
        }
        let temp = self.alloc_temp();
        self.compile_set_name(&temp, true)?;
        self.emit(Op::Pop);
        self.push_block_scope(&declarations);
        for function in functions {
            self.compile_function_value(&function)?;
            let name = function.name.clone().expect("named");
            self.compile_set_name(&name, true)?;
            self.emit(Op::Pop);
        }
        self.push_target(labels, TargetKind::Switch);
        let mut jumps = Vec::new();
        let mut default_case = None;
        for (position, case) in cases.iter().enumerate() {
            match &case.test {
                Some(test) => {
                    self.compile_get_name(&temp)?;
                    self.compile_expression(test)?;
                    self.emit(Op::Binary(BinaryOperator::StrictEqual));
                    jumps.push((position, self.emit(Op::JumpIfTrue(0))));
                }
                None => default_case = Some(position),
            }
        }
        let to_default = self.emit(Op::Jump(0));
        let mut starts = vec![0u32; cases.len()];
        for (position, case) in cases.iter().enumerate() {
            starts[position] = self.here();
            self.compile_statements(&case.body)?;
        }
        let end = self.here();
        for (position, at) in jumps {
            self.state().patch(at, starts[position]);
        }
        let default_target = default_case.map(|position| starts[position]).unwrap_or(end);
        self.state().patch(to_default, default_target);
        self.pop_target(None);
        self.pop_block_scope();
        Ok(())
    }

    fn compile_try(
        &mut self,
        block: &[Stmt],
        handler: Option<&CatchClause>,
        finalizer: Option<&[Stmt]>,
    ) -> Result<(), ScriptError> {
        let finally_handler = if finalizer.is_some() {
            let at = self.emit(Op::PushHandler {
                catch: NO_TARGET,
                finally: 0,
            });
            self.state().handler_depth += 1;
            Some(at)
        } else {
            None
        };
        if let Some(handler) = handler {
            let catch_handler = self.emit(Op::PushHandler {
                catch: NO_TARGET - 1,
                finally: NO_TARGET,
            });
            self.state().handler_depth += 1;
            self.compile_block(block)?;
            self.emit(Op::PopHandler);
            self.state().handler_depth -= 1;
            let skip = self.emit(Op::Jump(0));
            self.patch_here(catch_handler);
            // the thrown value is on the stack
            match &handler.param {
                Some(param) => {
                    let mut names = Vec::new();
                    bound_names(param, &mut names);
                    let declarations: Vec<_> = names
                        .into_iter()
                        .map(|name| (name, BindingKind::Let))
                        .collect();
                    self.push_block_scope(&declarations);
                    self.compile_binding(param, DeclarationKind::Let)?;
                    self.compile_block(&handler.body)?;
                    self.pop_block_scope();
                }
                None => {
                    self.emit(Op::Pop);
                    self.compile_block(&handler.body)?;
                }
            }
            self.patch_here(skip);
        } else {
            self.compile_block(block)?;
        }
        if let (Some(at), Some(finalizer)) = (finally_handler, finalizer) {
            self.emit(Op::PopHandler);
            self.state().handler_depth -= 1;
            self.emit(Op::CompletionNormal);
            self.patch_here(at);
            // a finally block that completes normally keeps the value the try gave
            let kept = match self.state().completion.clone() {
                Some(slot) => {
                    let kept = self.alloc_temp();
                    self.compile_get_name(&slot)?;
                    self.compile_set_name(&kept, true)?;
                    self.emit(Op::Pop);
                    Some((slot, kept))
                }
                None => None,
            };
            self.compile_block(finalizer)?;
            if let Some((slot, kept)) = kept {
                self.compile_get_name(&kept)?;
                self.compile_set_name(&slot, true)?;
                self.emit(Op::Pop);
            }
            self.emit(Op::EndFinally);
        }
        Ok(())
    }

    // ---- bindings and destructuring ----

    // binds the value on top of the stack to a declaration target, consuming it
    fn compile_binding(
        &mut self,
        pattern: &Pattern,
        kind: DeclarationKind,
    ) -> Result<(), ScriptError> {
        self.compile_pattern(pattern, Some(kind))
    }

    fn compile_assign_pattern(&mut self, pattern: &Pattern) -> Result<(), ScriptError> {
        self.compile_pattern(pattern, None)
    }

    fn compile_pattern(
        &mut self,
        pattern: &Pattern,
        declaration: Option<DeclarationKind>,
    ) -> Result<(), ScriptError> {
        let callsite = self.pattern_callsite.take();
        match pattern {
            Pattern::Identifier(name) => {
                if declaration == Some(DeclarationKind::Const) && self.resolve(name).is_none() {
                    let index = self.name(name);
                    self.emit(Op::InitGlobalConst(index));
                    self.emit(Op::Pop);
                    return Ok(());
                }
                let initialize = matches!(
                    declaration,
                    Some(DeclarationKind::Let | DeclarationKind::Const)
                );
                self.compile_set_name(name, initialize)?;
                self.emit(Op::Pop);
            }
            Pattern::Expression(target) => {
                // [value] -> evaluate the target reference, then store
                match target.as_ref() {
                    Expr::Member {
                        object, property, ..
                    } => {
                        self.compile_expression(object)?;
                        self.emit(Op::Swap);
                        let name = self.name(property);
                        self.emit(Op::SetProp(name));
                    }
                    Expr::Index { object, index, .. } => {
                        self.compile_expression(object)?;
                        self.compile_expression(index)?;
                        self.emit(Op::ToPropertyKey);
                        self.emit(Op::RotateUp3);
                        self.emit(Op::SetIndex);
                    }
                    Expr::PrivateMember { object, name, .. } => {
                        self.compile_expression(object)?;
                        self.compile_private_key(name)?;
                        self.emit(Op::RotateUp3);
                        self.emit(Op::SetPrivate);
                    }
                    Expr::SuperMember(PropertyKey::Static(name)) => {
                        self.compile_lexical(Lexical::This, Op::This);
                        self.compile_super_home();
                        self.emit(Op::RotateUp3);
                        let name = self.name(name);
                        self.emit(Op::SetSuper(name));
                    }
                    Expr::SuperMember(PropertyKey::Computed(key)) => {
                        self.compile_lexical(Lexical::This, Op::This);
                        self.compile_super_home();
                        self.compile_expression(key)?;
                        self.emit(Op::ToPropertyKey);
                        self.emit(Op::RotateUp4);
                        self.emit(Op::SetSuperIndex);
                    }
                    _ => return Err(error("invalid assignment target")),
                }
                self.emit(Op::Pop);
            }
            Pattern::Default { target, default } => {
                let skip = self.emit(Op::JumpIfNotUndefined(0));
                self.emit(Op::Pop);
                self.compile_named_expression(default, target)?;
                self.patch_here(skip);
                self.compile_pattern(target, declaration)?;
            }
            Pattern::Object { properties, rest } => {
                // destructuring a nullish value throws even with no properties
                self.emit(Op::RequireCoercible);
                // the source object sits in a temporary so each property reads from it
                let source = self.alloc_temp();
                self.compile_set_name(&source, true)?;
                self.emit(Op::Pop);
                let keys = if rest.is_some() {
                    let keys = self.alloc_temp();
                    self.emit(Op::NewArray);
                    self.compile_set_name(&keys, true)?;
                    self.emit(Op::Pop);
                    Some(keys)
                } else {
                    None
                };
                for (key, value) in properties {
                    self.compile_get_name(&source)?;
                    match key {
                        PropertyKey::Static(name) => {
                            if let Some(keys) = &keys {
                                self.compile_get_name(keys)?;
                                let index = self.name(name);
                                self.emit(Op::String(index));
                                self.emit(Op::ArrayPush);
                                self.emit(Op::Pop);
                            }
                            let name = self.name(name);
                            self.emit(Op::GetProp(name));
                        }
                        PropertyKey::Computed(expression) => {
                            self.compile_expression(expression)?;
                            self.emit(Op::ToPropertyKey);
                            if let Some(keys) = &keys {
                                self.emit(Op::Dup);
                                self.compile_get_name(keys)?;
                                self.emit(Op::Swap);
                                self.emit(Op::ArrayPush);
                                self.emit(Op::Pop);
                            }
                            self.emit(Op::GetIndex);
                        }
                        PropertyKey::Private(_) => return Err(error("invalid pattern key")),
                    }
                    self.compile_pattern(value, declaration)?;
                }
                if let (Some(rest), Some(keys)) = (rest, keys) {
                    self.compile_get_name(&source)?;
                    self.compile_get_name(&keys)?;
                    self.emit(Op::CopyRest);
                    self.compile_pattern(rest, declaration)?;
                }
            }
            Pattern::Array { elements, rest } => {
                self.emit(Op::GetIterator(callsite));
                for element in elements {
                    self.emit(Op::IterValue);
                    match element {
                        Some(element) => self.compile_pattern(element, declaration)?,
                        None => {
                            self.emit(Op::Pop);
                        }
                    }
                }
                if let Some(rest) = rest {
                    self.emit(Op::IterRest);
                    self.compile_pattern(rest, declaration)?;
                }
                self.emit(Op::IterClose);
            }
        }
        Ok(())
    }

    fn compile_private_key(&mut self, name: &Name) -> Result<(), ScriptError> {
        let hidden: Name = format!("#{name}").into();
        let name_index = self.name(&hidden);
        match self.resolve(&hidden) {
            Some((depth, index, _)) => {
                self.emit(Op::GetLocal {
                    depth,
                    index,
                    name: name_index,
                });
                Ok(())
            }
            None => Err(error(format!("private name #{name} is not defined"))),
        }
    }

    // anonymous functions and classes take the name of what they are bound to
    fn compile_named_expression(
        &mut self,
        expression: &Expr,
        target: &Pattern,
    ) -> Result<(), ScriptError> {
        if let Pattern::Identifier(name) = target {
            match expression {
                Expr::Function(function) if function.name.is_none() => {
                    let mut named = (**function).clone();
                    named.name = Some(name.clone());
                    return self.compile_function_value(&Rc::new(named));
                }
                Expr::Class(class) if class.name.is_none() => {
                    let mut named = (**class).clone();
                    named.name = Some(name.clone());
                    return self.compile_class_with_binding(&Rc::new(named), false);
                }
                _ => {}
            }
        }
        self.compile_expression(expression)
    }

    // ---- functions ----

    fn compile_function_value(&mut self, function: &Rc<FunctionNode>) -> Result<(), ScriptError> {
        let code = self.build_function(function)?;
        let named_expression = code.self_scope.is_some();
        let state = self.state();
        let index = state.children.len() as u32;
        state.children.push(code);
        self.emit(if named_expression {
            Op::ClosureNamed(index)
        } else {
            Op::Closure(index)
        });
        Ok(())
    }

    fn build_function(&mut self, function: &FunctionNode) -> Result<Rc<Code>, ScriptError> {
        self.build_function_with(function, false)
    }

    fn build_function_with(
        &mut self,
        function: &FunctionNode,
        named_expression: bool,
    ) -> Result<Rc<Code>, ScriptError> {
        let is_arrow = function.kind == FunctionKind::Arrow;
        let parent_strict = self.functions.last().is_some_and(|state| state.strict);
        let mut state = FunctionState::new(is_arrow, false, function.strict || parent_strict);
        state.async_generator = function.is_async && function.is_generator;
        if named_expression && let Some(name) = &function.name {
            // the self-name lives in a one-slot scope between the closure and its parent
            let mut outer = ScopeInfo::new(ScopeKind::Outer, true);
            outer.declare(name.clone(), BindingKind::Const);
            state.scopes.push(outer);
        }
        state.scopes.push(ScopeInfo::new(ScopeKind::Function, true));
        self.functions.push(state);
        // four reserved prologue slots, patched when arrows need lexical state
        for _ in 0..4 {
            self.emit(Op::Nop);
        }
        // with parameter expressions the body's vars live in a variable
        // environment of their own over the parameters, so closures made in the
        // parameter list never see them, as the function instantiation steps give
        let has_param_expressions = function
            .params
            .iter()
            .chain(function.rest.as_ref())
            .any(contains_expression);
        let simple_params = function.rest.is_none()
            && function
                .params
                .iter()
                .all(|param| matches!(param, Pattern::Identifier(_)));
        let mut param_slots = Vec::new();
        if simple_params {
            for param in &function.params {
                if let Pattern::Identifier(name) = param {
                    param_slots.push(Some(self.declare(name, BindingKind::Param)));
                }
            }
        } else {
            // the VM leaves every argument as an array on the stack for destructuring
            let mut names = Vec::new();
            for param in &function.params {
                bound_names(param, &mut names);
            }
            if let Some(rest) = &function.rest {
                bound_names(rest, &mut names);
            }
            for name in names {
                // parameters stay in the function scope under the body's own
                let index = self.state().function_scope_index();
                self.state().scopes[index].declare(name, BindingKind::Param);
            }
            self.emit(Op::GetIterator(None));
            for param in &function.params {
                self.emit(Op::IterValue);
                self.compile_pattern(param, Some(DeclarationKind::Let))?;
            }
            if let Some(rest) = &function.rest {
                self.emit(Op::IterRest);
                self.compile_pattern(rest, Some(DeclarationKind::Let))?;
            }
            self.emit(Op::IterClose);
        }
        // the parameters are evaluated against the frame's own scope; the body's
        // variable environment enters the chain only here
        if has_param_expressions {
            self.push_var_environment();
        }
        if !is_arrow
            && function_uses_arguments(&function.body, &function.params)
            && self
                .resolve("arguments")
                .is_none_or(|(depth, _, _)| depth > 0)
        {
            // the arguments object is written to the frame's own scope, whatever
            // environment the body's vars moved to
            let index = self.state().function_scope_index();
            let slot = self.state().scopes[index].declare("arguments".into(), BindingKind::Var);
            self.state().arguments_slot = Some(slot);
        }
        self.declare_function_body(&function.body)?;
        self.compile_statements(&function.body)?;
        // the body's variable environment only grows here, after every declaration
        // has taken its slot
        if let Some(target) = self.state().var_target {
            let layout = self.state().scopes[target].layout();
            let block = self.state().scopes[target].block as usize;
            self.state().blocks[block] = layout;
        }
        if function.kind == FunctionKind::DerivedConstructor
            || function.kind == FunctionKind::ClassConstructor
        {
            self.emit(Op::This);
        } else {
            self.emit(Op::Undefined);
        }
        self.emit(Op::Return);
        let state = self.functions.pop().expect("function state");
        Ok(Rc::new(self.finish(
            state,
            Some(function.source),
            function.kind,
            function.is_async,
            function.is_generator,
            function.name.as_deref().unwrap_or(""),
            function.length() as u32,
            named_expression,
            simple_params,
            param_slots,
        )))
    }

    // ---- classes ----

    fn compile_class(&mut self, class: &Rc<ClassNode>) -> Result<(), ScriptError> {
        self.compile_class_with_binding(class, true)
    }

    fn compile_class_with_binding(
        &mut self,
        class: &Rc<ClassNode>,
        _declaration: bool,
    ) -> Result<(), ScriptError> {
        // the class scope holds the inner class name and every private name of the body
        let mut declarations = Vec::new();
        if let Some(name) = &class.name {
            declarations.push((name.clone(), BindingKind::Const));
        }
        let mut private_names = Vec::new();
        for member in &class.members {
            if let PropertyKey::Private(name) = &member.key {
                let hidden: Name = format!("#{name}").into();
                if !private_names.contains(&hidden) {
                    private_names.push(hidden.clone());
                    declarations.push((hidden, BindingKind::Const));
                }
            }
        }
        self.push_block_scope(&declarations);
        for hidden in &private_names {
            let name = self.name(hidden);
            self.emit(Op::NewPrivateName(name));
            self.compile_set_name(hidden, true)?;
            self.emit(Op::Pop);
        }
        let has_super = class.superclass.is_some();
        if let Some(superclass) = &class.superclass {
            self.compile_expression(superclass)?;
        }
        let name = class.name.clone().unwrap_or_else(|| "".into());
        // the parser named the constructor method before a binding could name the class
        let constructor = match &class.constructor {
            Some(constructor) if constructor.name.as_ref() == Some(&name) => constructor.clone(),
            Some(constructor) => Rc::new(FunctionNode {
                name: Some(name.clone()),
                ..(**constructor).clone()
            }),
            None => Rc::new(default_constructor(&name, has_super, class.source)),
        };
        let constructor_code = self.build_function(&constructor)?;
        let template = Rc::new(ClassTemplate {
            constructor: constructor_code,
        });
        let state = self.state();
        let template_index = state.classes.len() as u32;
        state.classes.push(template);
        // [constructor prototype]
        self.emit(Op::Class {
            template: template_index,
            has_super,
        });
        let mut static_initializers = 0u16;
        for member in &class.members {
            let (computed, name_index) = match &member.key {
                PropertyKey::Static(name) => (false, self.name(name)),
                PropertyKey::Computed(expression) => {
                    self.compile_expression(expression)?;
                    self.emit(Op::ToPropertyKey);
                    (true, 0)
                }
                PropertyKey::Private(name) => {
                    self.compile_private_key(name)?;
                    (true, 0)
                }
            };
            let private = matches!(member.key, PropertyKey::Private(_));
            match member.kind {
                ClassMemberKind::Method | ClassMemberKind::Getter | ClassMemberKind::Setter => {
                    let function = member.function.as_ref().expect("methods have bodies");
                    self.compile_function_value(function)?;
                    let kind = match (member.kind, private) {
                        (ClassMemberKind::Getter, false) => GETTER,
                        (ClassMemberKind::Setter, false) => SETTER,
                        (_, false) => METHOD,
                        (ClassMemberKind::Getter, true) => PRIVATE_GETTER,
                        (ClassMemberKind::Setter, true) => PRIVATE_SETTER,
                        (_, true) => PRIVATE_METHOD,
                    };
                    self.emit(Op::DefineMethod {
                        is_static: member.is_static,
                        kind,
                        computed,
                        name: name_index,
                    });
                }
                ClassMemberKind::Field => {
                    match &member.function {
                        Some(function) => self.compile_function_value(function)?,
                        None => {
                            self.emit(Op::Undefined);
                        }
                    }
                    self.emit(Op::AddField {
                        is_static: member.is_static,
                        private,
                        computed,
                        name: name_index,
                    });
                    if member.is_static {
                        static_initializers += 1;
                    }
                }
                ClassMemberKind::StaticBlock => {
                    let function = member.function.as_ref().expect("static blocks have bodies");
                    self.compile_function_value(function)?;
                    self.emit(Op::AddField {
                        is_static: true,
                        private: false,
                        computed: false,
                        name: u32::MAX,
                    });
                    static_initializers += 1;
                }
            }
        }
        self.emit(Op::Pop);
        // the inner name is bound before static fields and blocks run, so they can use it
        if let Some(name) = &class.name {
            self.compile_set_name(name, true)?;
        }
        if static_initializers > 0 {
            self.emit(Op::RunStaticInit);
        }
        self.pop_block_scope();
        Ok(())
    }

    // ---- expressions ----

    fn compile_expression(&mut self, expression: &Expr) -> Result<(), ScriptError> {
        match expression {
            Expr::Number(value) => {
                self.emit(Op::Number(*value));
            }
            Expr::BigInt(digits) => {
                let index = self.name(digits);
                self.emit(Op::BigInt(index));
            }
            Expr::String(value) => {
                let index = self.name(value);
                self.emit(Op::String(index));
            }
            Expr::Bool(true) => {
                self.emit(Op::True);
            }
            Expr::Bool(false) => {
                self.emit(Op::False);
            }
            Expr::Null => {
                self.emit(Op::Null);
            }
            Expr::Regex { pattern, flags } => {
                let pattern = self.name(pattern);
                let flags = self.name(flags);
                self.emit(Op::Regex { pattern, flags });
            }
            Expr::Template {
                quasis,
                expressions,
            } => {
                let mut count = 0u16;
                for (index, quasi) in quasis.iter().enumerate() {
                    if !quasi.is_empty() {
                        let name = self.name(quasi);
                        self.emit(Op::String(name));
                        count += 1;
                    }
                    if let Some(expression) = expressions.get(index) {
                        self.compile_expression(expression)?;
                        count += 1;
                    }
                }
                if count == 0 {
                    let name = self.name("");
                    self.emit(Op::String(name));
                } else {
                    self.emit(Op::TemplateConcat(count));
                }
            }
            Expr::TaggedTemplate {
                tag,
                cooked,
                raw,
                expressions,
            } => {
                self.compile_callee(tag)?;
                let state = self.state();
                let index = state.templates.len() as u32;
                state.templates.push(TemplateSite {
                    cooked: cooked.clone(),
                    raw: raw.clone(),
                });
                self.emit(Op::TemplateObject(index));
                for expression in expressions {
                    self.compile_expression(expression)?;
                }
                self.emit(Op::CallMethod(expressions.len() as u16 + 1));
            }
            Expr::Identifier(name) => self.compile_get_name(name)?,
            Expr::This => self.compile_lexical(Lexical::This, Op::This),
            Expr::NewTarget => self.compile_lexical(Lexical::NewTarget, Op::NewTarget),
            Expr::ImportMeta => {
                self.emit(Op::ImportMeta);
            }
            Expr::Array(elements) => {
                self.emit(Op::NewArray);
                for element in elements {
                    match element {
                        ArrayElement::Hole => {
                            self.emit(Op::ArrayHole);
                        }
                        ArrayElement::Item(item) => {
                            self.compile_expression(item)?;
                            self.emit(Op::ArrayPush);
                        }
                        ArrayElement::Spread(item) => {
                            self.compile_expression(item)?;
                            let callsite = self.iterable_callsite(item);
                            self.emit(Op::ArraySpread(Some(callsite)));
                        }
                    }
                }
            }
            Expr::Object(properties) => self.compile_object(properties)?,
            Expr::Function(function) => {
                let named_expression =
                    function.name.is_some() && function.kind == FunctionKind::Normal;
                let code = self.build_function_with(function, named_expression)?;
                let state = self.state();
                let index = state.children.len() as u32;
                state.children.push(code);
                self.emit(if named_expression {
                    Op::ClosureNamed(index)
                } else {
                    Op::Closure(index)
                });
            }
            Expr::Class(class) => self.compile_class_with_binding(class, false)?,
            Expr::Unary { operator, argument } => self.compile_unary(*operator, argument)?,
            Expr::Update {
                increment,
                prefix,
                target,
            } => self.compile_update(*increment, *prefix, target)?,
            Expr::Binary {
                operator,
                left,
                right,
            } => {
                self.compile_expression(left)?;
                self.compile_expression(right)?;
                self.emit(Op::Binary(*operator));
            }
            Expr::Logical {
                operator,
                left,
                right,
            } => {
                self.compile_expression(left)?;
                let jump = self.emit(match operator {
                    LogicalOperator::And => Op::JumpIfFalseKeep(0),
                    LogicalOperator::Or => Op::JumpIfTrueKeep(0),
                    LogicalOperator::Nullish => Op::JumpIfNotNullishKeep(0),
                });
                self.emit(Op::Pop);
                self.compile_expression(right)?;
                self.patch_here(jump);
            }
            Expr::Assign {
                operator,
                target,
                value,
            } => self.compile_assign(*operator, target, value)?,
            Expr::Conditional {
                test,
                consequent,
                alternate,
            } => {
                self.compile_expression(test)?;
                let otherwise = self.emit(Op::JumpIfFalse(0));
                self.compile_expression(consequent)?;
                let end = self.emit(Op::Jump(0));
                self.patch_here(otherwise);
                self.compile_expression(alternate)?;
                self.patch_here(end);
            }
            Expr::Call {
                callee,
                arguments,
                optional,
            } => {
                // a call of the global eval by name is the one direct eval, which
                // sees the calling frame's strictness
                let direct_eval = !*optional
                    && matches!(callee.as_ref(), Expr::Identifier(name) if &**name == "eval")
                    && self.resolve("eval").is_none();
                self.compile_callee(callee)?;
                if *optional {
                    // [this callee]: a nullish callee ends the chain
                    let skip = self.emit(Op::JumpIfNullishChain(0));
                    self.chain_jumps_push(skip);
                }
                self.compile_call_arguments(arguments, true, direct_eval)?;
            }
            Expr::New { callee, arguments } => {
                self.compile_expression(callee)?;
                if arguments
                    .iter()
                    .any(|argument| matches!(argument, Argument::Spread(_)))
                {
                    self.compile_spread_arguments(arguments)?;
                    self.emit(Op::NewSpread);
                } else {
                    for argument in arguments {
                        if let Argument::Value(value) = argument {
                            self.compile_expression(value)?;
                        }
                    }
                    self.emit(Op::New(arguments.len() as u16));
                }
            }
            Expr::Member {
                object,
                property,
                optional,
            } => {
                self.compile_expression(object)?;
                if *optional {
                    let skip = self.emit(Op::JumpIfNullishChain(0));
                    self.chain_jumps_push(skip);
                }
                let name = self.name(property);
                self.emit(Op::GetProp(name));
            }
            Expr::Index {
                object,
                index,
                optional,
            } => {
                self.compile_expression(object)?;
                if *optional {
                    let skip = self.emit(Op::JumpIfNullishChain(0));
                    self.chain_jumps_push(skip);
                }
                self.compile_expression(index)?;
                self.emit(Op::GetIndex);
            }
            Expr::PrivateMember {
                object,
                name,
                optional,
            } => {
                self.compile_expression(object)?;
                if *optional {
                    let skip = self.emit(Op::JumpIfNullishChain(0));
                    self.chain_jumps_push(skip);
                }
                self.compile_private_key(name)?;
                self.emit(Op::GetPrivate);
            }
            Expr::PrivateIn { name, object } => {
                self.compile_private_key(name)?;
                self.compile_expression(object)?;
                self.emit(Op::HasPrivate);
            }
            Expr::SuperMember(key) => {
                self.compile_lexical(Lexical::This, Op::This);
                self.compile_super_home();
                match key {
                    PropertyKey::Static(name) => {
                        let name = self.name(name);
                        self.emit(Op::GetSuper(name));
                    }
                    PropertyKey::Computed(expression) => {
                        self.compile_expression(expression)?;
                        self.emit(Op::GetSuperIndex);
                    }
                    PropertyKey::Private(_) => return Err(error("invalid super property")),
                }
                // [this value] -> [value]
                self.emit(Op::Swap);
                self.emit(Op::Pop);
            }
            Expr::SuperCall(arguments) => {
                if arguments
                    .iter()
                    .any(|argument| matches!(argument, Argument::Spread(_)))
                {
                    self.compile_spread_arguments(arguments)?;
                    self.emit(Op::SuperCallSpread);
                } else {
                    for argument in arguments {
                        if let Argument::Value(value) = argument {
                            self.compile_expression(value)?;
                        }
                    }
                    self.emit(Op::SuperCall(arguments.len() as u16));
                }
                // super() inside an arrow also initializes the enclosing constructor's this
                if self.in_arrow()
                    && let Some((depth, index)) = self.lexical_slot(Lexical::This)
                {
                    let name = self.name("this");
                    self.emit(Op::SetLocal { depth, index, name });
                }
            }
            Expr::OptionalChain(inner) => {
                self.emit(Op::ChainStart);
                let saved = std::mem::take(&mut self.chain_jumps);
                self.compile_expression(inner)?;
                self.emit(Op::ChainEnd);
                let jumps = std::mem::replace(&mut self.chain_jumps, saved);
                for at in jumps {
                    self.patch_here(at);
                }
            }
            Expr::Sequence(expressions) => {
                for (index, expression) in expressions.iter().enumerate() {
                    self.compile_expression(expression)?;
                    if index + 1 < expressions.len() {
                        self.emit(Op::Pop);
                    }
                }
            }
            Expr::Yield { argument, delegate } => {
                match argument {
                    Some(argument) => self.compile_expression(argument)?,
                    None => {
                        self.emit(Op::Undefined);
                    }
                }
                if *delegate && self.state().async_generator {
                    self.emit(Op::GetAsyncIterator);
                    self.emit(Op::Undefined);
                    let start = self.here();
                    self.emit(Op::DelegateCall);
                    self.emit(Op::Await);
                    let done = self.emit(Op::DelegateUnpack(0));
                    self.emit(Op::YieldDelegating);
                    self.emit(Op::Jump(start));
                    self.patch_here(done);
                } else if *delegate {
                    self.emit(Op::GetIterator(None));
                    self.emit(Op::Undefined);
                    let start = self.here();
                    let done = self.emit(Op::DelegateStep(0));
                    self.emit(Op::YieldDelegating);
                    self.emit(Op::Jump(start));
                    self.patch_here(done);
                } else {
                    if self.state().async_generator {
                        self.emit(Op::Await);
                    }
                    self.emit(Op::Yield);
                }
            }
            Expr::Await(argument) => {
                self.compile_expression(argument)?;
                self.emit(Op::Await);
            }
            Expr::DynamicImport { specifier, options } => {
                self.compile_expression(specifier)?;
                match options {
                    Some(options) => self.compile_expression(options)?,
                    None => {
                        self.emit(Op::Undefined);
                    }
                }
                self.emit(Op::DynamicImport);
            }
        }
        Ok(())
    }

    fn chain_jumps_push(&mut self, at: usize) {
        self.chain_jumps.push(at);
    }

    fn compile_super_home(&mut self) {
        if self.in_arrow()
            && let Some((depth, index)) = self.lexical_slot(Lexical::Home)
        {
            let name = self.name("super");
            self.emit(Op::GetLocal { depth, index, name });
        } else {
            self.emit(Op::Home);
        }
    }

    // pushes [this callee] for a call, so methods receive their object
    fn compile_callee(&mut self, callee: &Expr) -> Result<(), ScriptError> {
        match callee {
            Expr::Member {
                object,
                property,
                optional,
            } => {
                self.compile_expression(object)?;
                if *optional {
                    let skip = self.emit(Op::JumpIfNullishChain(0));
                    self.chain_jumps_push(skip);
                }
                self.emit(Op::Dup);
                let name = self.name(property);
                self.emit(Op::GetProp(name));
            }
            Expr::Index {
                object,
                index,
                optional,
            } => {
                self.compile_expression(object)?;
                if *optional {
                    let skip = self.emit(Op::JumpIfNullishChain(0));
                    self.chain_jumps_push(skip);
                }
                self.emit(Op::Dup);
                self.compile_expression(index)?;
                self.emit(Op::GetIndex);
            }
            Expr::PrivateMember {
                object,
                name,
                optional,
            } => {
                self.compile_expression(object)?;
                if *optional {
                    let skip = self.emit(Op::JumpIfNullishChain(0));
                    self.chain_jumps_push(skip);
                }
                self.emit(Op::Dup);
                self.compile_private_key(name)?;
                self.emit(Op::GetPrivate);
            }
            Expr::SuperMember(key) => {
                self.compile_lexical(Lexical::This, Op::This);
                self.compile_super_home();
                match key {
                    PropertyKey::Static(name) => {
                        let name = self.name(name);
                        self.emit(Op::GetSuper(name));
                    }
                    PropertyKey::Computed(expression) => {
                        self.compile_expression(expression)?;
                        self.emit(Op::GetSuperIndex);
                    }
                    PropertyKey::Private(_) => return Err(error("invalid super property")),
                }
            }
            Expr::OptionalChain(inner)
                if matches!(inner.as_ref(), Expr::Member { .. } | Expr::Index { .. }) =>
            {
                // (a?.b)() keeps a as the receiver once the chain completes
                self.emit(Op::ChainStart);
                let saved = std::mem::take(&mut self.chain_jumps);
                self.compile_callee(inner)?;
                self.emit(Op::ChainEnd);
                let jumps = std::mem::replace(&mut self.chain_jumps, saved);
                let done = self.emit(Op::Jump(0));
                for at in jumps {
                    self.patch_here(at);
                }
                // a short circuit leaves one undefined: give it an undefined receiver
                self.emit(Op::Undefined);
                self.emit(Op::Swap);
                self.patch_here(done);
            }
            Expr::Identifier(name) if !self.with_objects(name).is_empty() => {
                // a function found on a with object is called with that object as this
                let objects = self.with_objects(name);
                let ends = self.compile_with_lookups(name, &objects, |compiler, index| {
                    compiler.emit(Op::Dup);
                    compiler.emit(Op::GetProp(index));
                })?;
                self.emit(Op::Undefined);
                self.compile_get_name_direct(name)?;
                for end in ends {
                    self.patch_here(end);
                }
            }
            _ => {
                self.emit(Op::Undefined);
                self.compile_expression(callee)?;
            }
        }
        Ok(())
    }

    fn compile_call_arguments(
        &mut self,
        arguments: &[Argument],
        method: bool,
        direct_eval: bool,
    ) -> Result<(), ScriptError> {
        if arguments
            .iter()
            .any(|argument| matches!(argument, Argument::Spread(_)))
        {
            self.compile_spread_arguments(arguments)?;
            self.emit(if method {
                Op::CallMethodSpread
            } else {
                Op::CallSpread
            });
        } else {
            for argument in arguments {
                if let Argument::Value(value) = argument {
                    self.compile_expression(value)?;
                }
            }
            let count = arguments.len() as u16;
            self.emit(if direct_eval {
                Op::CallEval(count)
            } else if method {
                Op::CallMethod(count)
            } else {
                Op::Call(count)
            });
        }
        Ok(())
    }

    fn compile_spread_arguments(&mut self, arguments: &[Argument]) -> Result<(), ScriptError> {
        self.emit(Op::NewArray);
        for argument in arguments {
            match argument {
                Argument::Value(value) => {
                    self.compile_expression(value)?;
                    self.emit(Op::ArrayPush);
                }
                Argument::Spread(value) => {
                    self.compile_expression(value)?;
                    let site = callsite(value);
                    let site = self.name(&site);
                    self.emit(Op::ArgumentSpread(site));
                }
            }
        }
        Ok(())
    }

    fn compile_object(&mut self, properties: &[ObjectProperty]) -> Result<(), ScriptError> {
        self.emit(Op::NewObject);
        for property in properties {
            match property {
                ObjectProperty::Value(key, value) => match key {
                    PropertyKey::Static(name) => {
                        self.compile_named_expression(value, &Pattern::Identifier(name.clone()))?;
                        let name = self.name(name);
                        self.emit(Op::DefineField(name));
                    }
                    PropertyKey::Computed(expression) => {
                        self.compile_expression(expression)?;
                        self.emit(Op::ToPropertyKey);
                        self.compile_expression(value)?;
                        self.emit(Op::DefineComputed);
                    }
                    PropertyKey::Private(_) => return Err(error("invalid object key")),
                },
                ObjectProperty::Method {
                    key,
                    kind,
                    function,
                } => {
                    let computed = match key {
                        PropertyKey::Computed(expression) => {
                            self.compile_expression(expression)?;
                            self.emit(Op::ToPropertyKey);
                            true
                        }
                        _ => false,
                    };
                    let name = property_name(key).map(|name| self.name(name)).unwrap_or(0);
                    self.compile_function_value(function)?;
                    match kind {
                        MethodKind::Method => {
                            self.emit(Op::DefineLiteralMethod { computed, name });
                        }
                        MethodKind::Getter | MethodKind::Setter => {
                            self.emit(Op::DefineAccessor {
                                getter: *kind == MethodKind::Getter,
                                computed,
                                name,
                            });
                        }
                    }
                }
                ObjectProperty::Spread(value) => {
                    self.compile_expression(value)?;
                    self.emit(Op::ObjectSpread);
                }
                ObjectProperty::Prototype(value) => {
                    self.compile_expression(value)?;
                    self.emit(Op::SetPrototype);
                }
            }
        }
        Ok(())
    }

    fn compile_unary(
        &mut self,
        operator: UnaryOperator,
        argument: &Expr,
    ) -> Result<(), ScriptError> {
        match operator {
            UnaryOperator::TypeOf => {
                if let Expr::Identifier(name) = argument
                    && !self.with_objects(name).is_empty()
                {
                    let objects = self.with_objects(name);
                    let ends = self.compile_with_lookups(name, &objects, |compiler, index| {
                        compiler.emit(Op::GetProp(index));
                        compiler.emit(Op::TypeOf);
                    })?;
                    if self.resolve(name).is_none() {
                        let index = self.name(name);
                        self.emit(Op::TypeofGlobal(index));
                    } else {
                        self.compile_get_name_direct(name)?;
                        self.emit(Op::TypeOf);
                    }
                    for end in ends {
                        self.patch_here(end);
                    }
                } else if let Expr::Identifier(name) = argument
                    && self.resolve(name).is_none()
                    && &**name != "arguments"
                {
                    let index = self.name(name);
                    self.emit(Op::TypeofGlobal(index));
                } else {
                    self.compile_expression(argument)?;
                    self.emit(Op::TypeOf);
                }
            }
            UnaryOperator::Delete => match argument {
                Expr::Member {
                    object, property, ..
                } => {
                    self.compile_expression(object)?;
                    let name = self.name(property);
                    self.emit(Op::DeleteProp(name));
                }
                Expr::Index { object, index, .. } => {
                    self.compile_expression(object)?;
                    self.compile_expression(index)?;
                    self.emit(Op::DeleteIndex);
                }
                Expr::OptionalChain(inner) => {
                    self.emit(Op::ChainStart);
                    let saved = std::mem::take(&mut self.chain_jumps);
                    self.compile_unary(UnaryOperator::Delete, inner)?;
                    self.emit(Op::ChainEnd);
                    let jumps = std::mem::replace(&mut self.chain_jumps, saved);
                    let done = self.emit(Op::Jump(0));
                    for at in jumps {
                        self.patch_here(at);
                    }
                    // a short-circuited delete is true
                    self.emit(Op::Pop);
                    self.emit(Op::True);
                    self.patch_here(done);
                }
                Expr::Identifier(name) if !self.with_objects(name).is_empty() => {
                    let objects = self.with_objects(name);
                    let ends = self.compile_with_lookups(name, &objects, |compiler, index| {
                        compiler.emit(Op::DeleteProp(index));
                    })?;
                    if self.resolve(name).is_some() {
                        self.emit(Op::False);
                    } else {
                        let index = self.name(name);
                        self.emit(Op::DeleteGlobal(index));
                    }
                    for end in ends {
                        self.patch_here(end);
                    }
                }
                Expr::Identifier(name) => {
                    match self.resolve(name) {
                        // a binding a direct eval grew past its scope's layout is
                        // deletable, as the eval declaration steps made it
                        Some((depth, index, _)) if self.grown_binding(depth, index) => {
                            self.emit(Op::DeleteGrown { depth, index });
                        }
                        Some(_) => {
                            self.emit(Op::False);
                        }
                        None => {
                            let index = self.name(name);
                            self.emit(Op::DeleteGlobal(index));
                        }
                    }
                }
                other => {
                    self.compile_expression(other)?;
                    self.emit(Op::Pop);
                    self.emit(Op::True);
                }
            },
            UnaryOperator::Void => {
                self.compile_expression(argument)?;
                self.emit(Op::Pop);
                self.emit(Op::Undefined);
            }
            UnaryOperator::Not => {
                self.compile_expression(argument)?;
                self.emit(Op::Not);
            }
            UnaryOperator::Negative => {
                self.compile_expression(argument)?;
                self.emit(Op::Negate);
            }
            UnaryOperator::Positive => {
                self.compile_expression(argument)?;
                self.emit(Op::Plus);
            }
            UnaryOperator::BitNot => {
                self.compile_expression(argument)?;
                self.emit(Op::BitNot);
            }
        }
        Ok(())
    }

    // loads a reference's current value, leaving what is needed to store it back underneath
    fn compile_reference_load(&mut self, target: &Expr) -> Result<RefKind, ScriptError> {
        Ok(match target {
            Expr::Identifier(name) => {
                self.compile_get_name(name)?;
                RefKind::Name(name.clone())
            }
            Expr::Member {
                object, property, ..
            } => {
                self.compile_expression(object)?;
                self.emit(Op::Dup);
                let name = self.name(property);
                self.emit(Op::GetProp(name));
                RefKind::Member(name)
            }
            Expr::Index { object, index, .. } => {
                self.compile_expression(object)?;
                self.compile_expression(index)?;
                self.emit(Op::ToPropertyKey);
                self.emit(Op::Dup2);
                self.emit(Op::GetIndex);
                RefKind::Index
            }
            Expr::PrivateMember { object, name, .. } => {
                self.compile_expression(object)?;
                self.compile_private_key(name)?;
                self.emit(Op::Dup2);
                self.emit(Op::GetPrivate);
                RefKind::Private
            }
            Expr::SuperMember(key) => {
                self.compile_lexical(Lexical::This, Op::This);
                self.compile_super_home();
                match key {
                    PropertyKey::Static(name) => {
                        let name = self.name(name);
                        // [this home] -> [this home this home] -> [this home this value]
                        self.emit(Op::Dup2);
                        self.emit(Op::GetSuper(name));
                        // -> [this home value]
                        self.emit(Op::Swap);
                        self.emit(Op::Pop);
                        RefKind::Super(name)
                    }
                    PropertyKey::Computed(key) => {
                        self.compile_expression(key)?;
                        self.emit(Op::ToPropertyKey);
                        // [this home key] -> [this home key this value] -> [this home key value]
                        self.emit(Op::Dup3);
                        self.emit(Op::GetSuperIndex);
                        self.emit(Op::Swap);
                        self.emit(Op::Pop);
                        RefKind::SuperIndex
                    }
                    PropertyKey::Private(_) => return Err(error("invalid assignment target")),
                }
            }
            _ => return Err(error("invalid assignment target")),
        })
    }

    // [reference parts..., value] -> [value], storing it into the reference
    fn compile_reference_store(&mut self, kind: &RefKind) -> Result<(), ScriptError> {
        match kind {
            RefKind::Name(name) => self.compile_set_name(name, false)?,
            RefKind::Member(name) => {
                self.emit(Op::SetProp(*name));
            }
            RefKind::Index => {
                self.emit(Op::SetIndex);
            }
            RefKind::Private => {
                self.emit(Op::SetPrivate);
            }
            RefKind::Super(name) => {
                self.emit(Op::SetSuper(*name));
            }
            RefKind::SuperIndex => {
                self.emit(Op::SetSuperIndex);
            }
        }
        Ok(())
    }

    fn compile_assign(
        &mut self,
        operator: AssignOperator,
        target: &Pattern,
        value: &Expr,
    ) -> Result<(), ScriptError> {
        match operator {
            AssignOperator::Assign => match target {
                Pattern::Identifier(name) => {
                    self.compile_named_expression(value, target)?;
                    self.compile_set_name(name, false)?;
                }
                Pattern::Expression(expression) => match expression.as_ref() {
                    Expr::Member {
                        object, property, ..
                    } => {
                        self.compile_expression(object)?;
                        self.compile_expression(value)?;
                        let name = self.name(property);
                        self.emit(Op::SetProp(name));
                    }
                    Expr::Index { object, index, .. } => {
                        self.compile_expression(object)?;
                        self.compile_expression(index)?;
                        self.emit(Op::ToPropertyKey);
                        self.compile_expression(value)?;
                        self.emit(Op::SetIndex);
                    }
                    Expr::PrivateMember { object, name, .. } => {
                        self.compile_expression(object)?;
                        self.compile_private_key(name)?;
                        self.compile_expression(value)?;
                        self.emit(Op::SetPrivate);
                    }
                    _ => {
                        self.compile_expression(value)?;
                        self.emit(Op::Dup);
                        self.compile_assign_pattern(target)?;
                    }
                },
                _ => {
                    // destructuring assignment evaluates to the right-hand value
                    self.compile_expression(value)?;
                    self.emit(Op::Dup);
                    self.compile_assign_pattern(target)?;
                }
            },
            AssignOperator::Binary(binary) => {
                let Pattern::Expression(expression) = target else {
                    let Pattern::Identifier(name) = target else {
                        return Err(error("invalid compound assignment"));
                    };
                    self.compile_get_name(name)?;
                    self.compile_expression(value)?;
                    self.emit(Op::Binary(binary));
                    self.compile_set_name(name, false)?;
                    return Ok(());
                };
                let kind = self.compile_reference_load(expression)?;
                self.compile_expression(value)?;
                self.emit(Op::Binary(binary));
                self.compile_reference_store(&kind)?;
            }
            AssignOperator::Logical(logical) => {
                let expression = match target {
                    Pattern::Identifier(name) => Expr::Identifier(name.clone()),
                    Pattern::Expression(expression) => (**expression).clone(),
                    _ => return Err(error("invalid logical assignment")),
                };
                let kind = self.compile_reference_load(&expression)?;
                let jump = self.emit(match logical {
                    LogicalOperator::And => Op::JumpIfFalseKeep(0),
                    LogicalOperator::Or => Op::JumpIfTrueKeep(0),
                    LogicalOperator::Nullish => Op::JumpIfNotNullishKeep(0),
                });
                self.emit(Op::Pop);
                match target {
                    Pattern::Identifier(_) => self.compile_named_expression(value, target)?,
                    _ => self.compile_expression(value)?,
                }
                self.compile_reference_store(&kind)?;
                let end = self.emit(Op::Jump(0));
                self.patch_here(jump);
                // the short circuit leaves the reference parts under the kept value
                match kind {
                    RefKind::Name(_) => {}
                    RefKind::Member(_) => {
                        self.emit(Op::Swap);
                        self.emit(Op::Pop);
                    }
                    RefKind::Index | RefKind::Private | RefKind::Super(_) => {
                        self.emit(Op::RotateDown3);
                        self.emit(Op::Pop);
                        self.emit(Op::Pop);
                    }
                    RefKind::SuperIndex => {
                        self.emit(Op::RotateDown4);
                        self.emit(Op::Pop);
                        self.emit(Op::Pop);
                        self.emit(Op::Pop);
                    }
                }
                self.patch_here(end);
            }
        }
        Ok(())
    }

    fn compile_update(
        &mut self,
        increment: bool,
        prefix: bool,
        target: &Expr,
    ) -> Result<(), ScriptError> {
        let step = if increment {
            Op::Increment
        } else {
            Op::Decrement
        };
        let kind = self.compile_reference_load(target)?;
        self.emit(Op::ToNumeric);
        if prefix {
            self.emit(step);
            self.compile_reference_store(&kind)?;
            return Ok(());
        }
        // postfix keeps the old value: stash it below the reference parts
        let parts = match kind {
            RefKind::Name(_) => 0,
            RefKind::Member(_) => 1,
            RefKind::Index | RefKind::Private | RefKind::Super(_) => 2,
            RefKind::SuperIndex => 3,
        };
        self.emit(Op::Dup);
        match parts {
            0 => {
                // [old old] -> [old new]
            }
            1 => {
                // [obj old old] -> [old obj old]
                self.emit(Op::RotateDown3);
            }
            2 => {
                // [obj key old old] -> [old obj key old]
                self.emit(Op::RotateDown4);
            }
            _ => {
                // [this home key old old] -> [old this home key old]
                self.emit(Op::RotateDown5);
            }
        }
        self.emit(step);
        self.compile_reference_store(&kind)?;
        self.emit(Op::Pop);
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lexical {
    This,
    Arguments,
    NewTarget,
    Home,
}

enum RefKind {
    Name(Name),
    Member(u32),
    Index,
    Private,
    Super(u32),
    SuperIndex,
}

fn default_constructor(name: &Name, derived: bool, source: (usize, usize)) -> FunctionNode {
    let (params, rest, body) = if derived {
        (
            Vec::new(),
            Some(Pattern::Identifier("args".into())),
            vec![Stmt::Expression(Expr::SuperCall(vec![Argument::Spread(
                Expr::Identifier("args".into()),
            )]))],
        )
    } else {
        (Vec::new(), None, Vec::new())
    };
    FunctionNode {
        name: Some(name.clone()),
        params,
        rest,
        body,
        kind: if derived {
            FunctionKind::DerivedConstructor
        } else {
            FunctionKind::ClassConstructor
        },
        is_async: false,
        is_generator: false,
        strict: true,
        source,
    }
}

// whether binding a pattern runs code: a default or a computed key, at any depth
fn contains_expression(pattern: &Pattern) -> bool {
    match pattern {
        Pattern::Identifier(_) => false,
        Pattern::Default { .. } | Pattern::Expression(_) => true,
        Pattern::Object { properties, rest } => {
            properties.iter().any(|(key, value)| {
                matches!(key, PropertyKey::Computed(_)) || contains_expression(value)
            }) || rest.as_deref().is_some_and(contains_expression)
        }
        Pattern::Array { elements, rest } => {
            elements.iter().flatten().any(contains_expression)
                || rest.as_deref().is_some_and(contains_expression)
        }
    }
}

// whether a function body reads the arguments object, looking through arrows but not functions
fn function_uses_arguments(body: &[Stmt], params: &[Pattern]) -> bool {
    let mut found = false;
    let mut visit = |expression: &Expr| {
        if matches!(expression, Expr::Identifier(name) if &**name == "arguments") {
            found = true;
        }
    };
    for statement in body {
        walk_statement(statement, &mut visit);
    }
    for param in params {
        walk_pattern(param, &mut visit);
    }
    found
}

fn walk_pattern(pattern: &Pattern, visit: &mut dyn FnMut(&Expr)) {
    match pattern {
        Pattern::Identifier(_) => {}
        Pattern::Expression(expression) => walk_expression(expression, visit),
        Pattern::Object { properties, rest } => {
            for (key, value) in properties {
                if let PropertyKey::Computed(key) = key {
                    walk_expression(key, visit);
                }
                walk_pattern(value, visit);
            }
            if let Some(rest) = rest {
                walk_pattern(rest, visit);
            }
        }
        Pattern::Array { elements, rest } => {
            for element in elements.iter().flatten() {
                walk_pattern(element, visit);
            }
            if let Some(rest) = rest {
                walk_pattern(rest, visit);
            }
        }
        Pattern::Default { target, default } => {
            walk_pattern(target, visit);
            walk_expression(default, visit);
        }
    }
}

fn walk_statement(statement: &Stmt, visit: &mut dyn FnMut(&Expr)) {
    match statement {
        Stmt::Expression(expression) | Stmt::Throw(expression) => {
            walk_expression(expression, visit)
        }
        Stmt::Return(Some(expression)) => walk_expression(expression, visit),
        Stmt::Block(body) => body
            .iter()
            .for_each(|statement| walk_statement(statement, visit)),
        Stmt::Declaration { declarators, .. } => {
            for declarator in declarators {
                walk_pattern(&declarator.target, visit);
                if let Some(init) = &declarator.init {
                    walk_expression(init, visit);
                }
            }
        }
        Stmt::If {
            test,
            consequent,
            alternate,
        } => {
            walk_expression(test, visit);
            walk_statement(consequent, visit);
            if let Some(alternate) = alternate {
                walk_statement(alternate, visit);
            }
        }
        Stmt::For {
            init,
            test,
            update,
            body,
        } => {
            match init {
                Some(ForInit::Expression(expression)) => walk_expression(expression, visit),
                Some(ForInit::Declaration { declarators, .. }) => {
                    for declarator in declarators {
                        if let Some(init) = &declarator.init {
                            walk_expression(init, visit);
                        }
                    }
                }
                None => {}
            }
            if let Some(test) = test {
                walk_expression(test, visit);
            }
            if let Some(update) = update {
                walk_expression(update, visit);
            }
            walk_statement(body, visit);
        }
        Stmt::ForIn { right, body, .. } | Stmt::ForOf { right, body, .. } => {
            walk_expression(right, visit);
            walk_statement(body, visit);
        }
        Stmt::While { test, body }
        | Stmt::DoWhile { body, test }
        | Stmt::With { object: test, body } => {
            walk_expression(test, visit);
            walk_statement(body, visit);
        }
        Stmt::Switch {
            discriminant,
            cases,
        } => {
            walk_expression(discriminant, visit);
            for case in cases {
                if let Some(test) = &case.test {
                    walk_expression(test, visit);
                }
                case.body
                    .iter()
                    .for_each(|statement| walk_statement(statement, visit));
            }
        }
        Stmt::Labeled { body, .. } => walk_statement(body, visit),
        Stmt::Try {
            block,
            handler,
            finalizer,
        } => {
            block
                .iter()
                .for_each(|statement| walk_statement(statement, visit));
            if let Some(handler) = handler {
                handler
                    .body
                    .iter()
                    .for_each(|statement| walk_statement(statement, visit));
            }
            if let Some(finalizer) = finalizer {
                finalizer
                    .iter()
                    .for_each(|statement| walk_statement(statement, visit));
            }
        }
        _ => {}
    }
}

fn walk_expression(expression: &Expr, visit: &mut dyn FnMut(&Expr)) {
    visit(expression);
    let mut children: Vec<&Expr> = Vec::new();
    match expression {
        Expr::Function(function) if function.kind == FunctionKind::Arrow => {
            function
                .body
                .iter()
                .for_each(|statement| walk_statement(statement, visit));
        }
        Expr::Template { expressions, .. } => children.extend(expressions),
        Expr::TaggedTemplate {
            tag, expressions, ..
        } => {
            children.push(tag);
            children.extend(expressions);
        }
        Expr::Array(elements) => {
            for element in elements {
                match element {
                    ArrayElement::Item(item) | ArrayElement::Spread(item) => children.push(item),
                    ArrayElement::Hole => {}
                }
            }
        }
        Expr::Object(properties) => {
            for property in properties {
                match property {
                    ObjectProperty::Value(key, value) => {
                        if let PropertyKey::Computed(key) = key {
                            children.push(key);
                        }
                        children.push(value);
                    }
                    ObjectProperty::Spread(value) | ObjectProperty::Prototype(value) => {
                        children.push(value)
                    }
                    ObjectProperty::Method { key, .. } => {
                        if let PropertyKey::Computed(key) = key {
                            children.push(key);
                        }
                    }
                }
            }
        }
        Expr::Unary { argument, .. } | Expr::Await(argument) => children.push(argument),
        Expr::Update { target, .. } => children.push(target),
        Expr::Binary { left, right, .. } | Expr::Logical { left, right, .. } => {
            children.push(left);
            children.push(right);
        }
        Expr::Assign { target, value, .. } => {
            walk_pattern(target, visit);
            children.push(value);
        }
        Expr::Conditional {
            test,
            consequent,
            alternate,
        } => {
            children.push(test);
            children.push(consequent);
            children.push(alternate);
        }
        Expr::Call {
            callee, arguments, ..
        }
        | Expr::New { callee, arguments } => {
            children.push(callee);
            for argument in arguments {
                match argument {
                    Argument::Value(value) | Argument::Spread(value) => children.push(value),
                }
            }
        }
        Expr::SuperCall(arguments) => {
            for argument in arguments {
                match argument {
                    Argument::Value(value) | Argument::Spread(value) => children.push(value),
                }
            }
        }
        Expr::Member { object, .. }
        | Expr::PrivateMember { object, .. }
        | Expr::PrivateIn { object, .. } => children.push(object),
        Expr::Index { object, index, .. } => {
            children.push(object);
            children.push(index);
        }
        Expr::SuperMember(PropertyKey::Computed(key)) => children.push(key),
        Expr::OptionalChain(inner) => children.push(inner),
        Expr::Sequence(expressions) => children.extend(expressions),
        Expr::Yield {
            argument: Some(argument),
            ..
        } => children.push(argument),
        Expr::DynamicImport { specifier, options } => {
            children.push(specifier);
            if let Some(options) = options {
                children.push(options);
            }
        }
        Expr::Class(class) => {
            if let Some(superclass) = &class.superclass {
                children.push(superclass);
            }
        }
        _ => {}
    }
    for child in children {
        walk_expression(child, visit);
    }
}
