// the syntax tree for ECMAScript 2022 scripts and modules; function and class bodies
// sit behind Rc so a closure shares its code instead of copying it

use std::rc::Rc;

pub(super) type Name = Rc<str>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DeclarationKind {
    Var,
    Let,
    Const,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Declarator {
    pub(super) target: Pattern,
    pub(super) init: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Stmt {
    Empty,
    Expression(Expr),
    Block(Vec<Stmt>),
    Declaration {
        kind: DeclarationKind,
        declarators: Vec<Declarator>,
    },
    Function(Rc<FunctionNode>),
    Class(Rc<ClassNode>),
    If {
        test: Expr,
        consequent: Box<Stmt>,
        alternate: Option<Box<Stmt>>,
    },
    For {
        init: Option<ForInit>,
        test: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
    },
    ForIn {
        left: ForHead,
        right: Expr,
        body: Box<Stmt>,
    },
    ForOf {
        left: ForHead,
        right: Expr,
        body: Box<Stmt>,
        is_await: bool,
    },
    While {
        test: Expr,
        body: Box<Stmt>,
    },
    DoWhile {
        body: Box<Stmt>,
        test: Expr,
    },
    Switch {
        discriminant: Expr,
        cases: Vec<SwitchCase>,
    },
    Break(Option<Name>),
    Continue(Option<Name>),
    Labeled {
        label: Name,
        body: Box<Stmt>,
    },
    Return(Option<Expr>),
    Throw(Expr),
    Try {
        block: Vec<Stmt>,
        handler: Option<CatchClause>,
        finalizer: Option<Vec<Stmt>>,
    },
    Debugger,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ForInit {
    Declaration {
        kind: DeclarationKind,
        declarators: Vec<Declarator>,
    },
    Expression(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ForHead {
    Declaration {
        kind: DeclarationKind,
        target: Pattern,
    },
    Target(Pattern),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct SwitchCase {
    pub(super) test: Option<Expr>,
    pub(super) body: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct CatchClause {
    pub(super) param: Option<Pattern>,
    pub(super) body: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum PropertyKey {
    Static(Name),
    Computed(Box<Expr>),
    Private(Name),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Pattern {
    Identifier(Name),
    // an assignment target such as a.b or a[i], valid in destructuring assignment
    Expression(Box<Expr>),
    Object {
        properties: Vec<(PropertyKey, Pattern)>,
        rest: Option<Box<Pattern>>,
    },
    Array {
        elements: Vec<Option<Pattern>>,
        rest: Option<Box<Pattern>>,
    },
    Default {
        target: Box<Pattern>,
        default: Box<Expr>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnaryOperator {
    Not,
    Negative,
    Positive,
    BitNot,
    TypeOf,
    Void,
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BinaryOperator {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Exponent,
    ShiftLeft,
    ShiftRight,
    ShiftRightUnsigned,
    BitAnd,
    BitOr,
    BitXor,
    Equal,
    NotEqual,
    StrictEqual,
    StrictNotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    InstanceOf,
    In,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LogicalOperator {
    And,
    Or,
    Nullish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AssignOperator {
    Assign,
    Binary(BinaryOperator),
    Logical(LogicalOperator),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ArrayElement {
    Hole,
    Item(Expr),
    Spread(Expr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MethodKind {
    Method,
    Getter,
    Setter,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ObjectProperty {
    Value(PropertyKey, Expr),
    Method {
        key: PropertyKey,
        kind: MethodKind,
        function: Rc<FunctionNode>,
    },
    Spread(Expr),
    // "__proto__: value" sets the prototype of the new object
    Prototype(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Argument {
    Value(Expr),
    Spread(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Expr {
    Number(f64),
    BigInt(Name),
    String(Name),
    Bool(bool),
    Null,
    Regex {
        pattern: Name,
        flags: Name,
    },
    Template {
        quasis: Vec<Name>,
        expressions: Vec<Expr>,
    },
    TaggedTemplate {
        tag: Box<Expr>,
        cooked: Vec<Option<Name>>,
        raw: Vec<Name>,
        expressions: Vec<Expr>,
    },
    Identifier(Name),
    This,
    NewTarget,
    ImportMeta,
    Array(Vec<ArrayElement>),
    Object(Vec<ObjectProperty>),
    Function(Rc<FunctionNode>),
    Class(Rc<ClassNode>),
    Unary {
        operator: UnaryOperator,
        argument: Box<Expr>,
    },
    Update {
        increment: bool,
        prefix: bool,
        target: Box<Expr>,
    },
    Binary {
        operator: BinaryOperator,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Logical {
        operator: LogicalOperator,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Assign {
        operator: AssignOperator,
        target: Box<Pattern>,
        value: Box<Expr>,
    },
    Conditional {
        test: Box<Expr>,
        consequent: Box<Expr>,
        alternate: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        arguments: Vec<Argument>,
        optional: bool,
    },
    New {
        callee: Box<Expr>,
        arguments: Vec<Argument>,
    },
    Member {
        object: Box<Expr>,
        property: Name,
        optional: bool,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
        optional: bool,
    },
    PrivateMember {
        object: Box<Expr>,
        name: Name,
        optional: bool,
    },
    PrivateIn {
        name: Name,
        object: Box<Expr>,
    },
    SuperMember(PropertyKey),
    SuperCall(Vec<Argument>),
    // the extent of an optional chain: a short circuit anywhere inside yields undefined
    OptionalChain(Box<Expr>),
    Sequence(Vec<Expr>),
    Yield {
        argument: Option<Box<Expr>>,
        delegate: bool,
    },
    Await(Box<Expr>),
    DynamicImport {
        specifier: Box<Expr>,
        options: Option<Box<Expr>>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FunctionKind {
    Normal,
    Arrow,
    Method,
    Getter,
    Setter,
    ClassConstructor,
    DerivedConstructor,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct FunctionNode {
    pub(super) name: Option<Name>,
    pub(super) params: Vec<Pattern>,
    pub(super) rest: Option<Pattern>,
    pub(super) body: Vec<Stmt>,
    pub(super) kind: FunctionKind,
    pub(super) is_async: bool,
    pub(super) is_generator: bool,
    pub(super) strict: bool,
    // the source text, which Function.prototype.toString returns
    pub(super) source: (usize, usize),
}

impl FunctionNode {
    // the length property: parameters before the first default or rest
    pub(super) fn length(&self) -> usize {
        self.params
            .iter()
            .take_while(|param| !matches!(param, Pattern::Default { .. }))
            .count()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClassMemberKind {
    Method,
    Getter,
    Setter,
    Field,
    StaticBlock,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ClassMember {
    pub(super) key: PropertyKey,
    pub(super) is_static: bool,
    pub(super) kind: ClassMemberKind,
    // the method body, or a field initializer or static block wrapped as a method
    pub(super) function: Option<Rc<FunctionNode>>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ClassNode {
    pub(super) name: Option<Name>,
    pub(super) superclass: Option<Expr>,
    pub(super) constructor: Option<Rc<FunctionNode>>,
    pub(super) members: Vec<ClassMember>,
    pub(super) source: (usize, usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ImportBinding {
    Named { imported: Name, local: Name },
    Namespace(Name),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ImportDeclaration {
    pub(super) specifier: Name,
    pub(super) bindings: Vec<ImportBinding>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ExportDeclaration {
    // export { local as exported }, optionally from another module
    Named {
        bindings: Vec<(Name, Name)>,
        from: Option<Name>,
    },
    // export * from "x", or export * as name from "x"
    All {
        from: Name,
        alias: Option<Name>,
    },
    // a declaration whose bound names are exported under the same names
    Declaration(Stmt),
    // export default: the value lands in a synthetic local binding
    Default {
        local: Name,
        statement: Stmt,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ModuleItem {
    Statement(Stmt),
    Import(ImportDeclaration),
    Export(ExportDeclaration),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Script {
    pub(super) body: Vec<Stmt>,
    pub(super) strict: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Module {
    pub(super) items: Vec<ModuleItem>,
}

// the names a declaration statement binds, used for hoisting and exports
pub(super) fn bound_names(pattern: &Pattern, names: &mut Vec<Name>) {
    match pattern {
        Pattern::Identifier(name) => names.push(name.clone()),
        Pattern::Expression(_) => {}
        Pattern::Object { properties, rest } => {
            for (_, value) in properties {
                bound_names(value, names);
            }
            if let Some(rest) = rest {
                bound_names(rest, names);
            }
        }
        Pattern::Array { elements, rest } => {
            for element in elements.iter().flatten() {
                bound_names(element, names);
            }
            if let Some(rest) = rest {
                bound_names(rest, names);
            }
        }
        Pattern::Default { target, .. } => bound_names(target, names),
    }
}
