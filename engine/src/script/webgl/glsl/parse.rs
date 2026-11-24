// the GLSL ES grammar: declarations, functions, statements and expressions as an AST, with
// type names told from identifiers by the built-in type words and the structs declared so far

use super::lex::{Tok, Token};
use crate::hashing::HashSet;

pub(crate) const TYPE_WORDS: &[&str] = &[
    "void",
    "bool",
    "int",
    "uint",
    "float",
    "vec2",
    "vec3",
    "vec4",
    "bvec2",
    "bvec3",
    "bvec4",
    "ivec2",
    "ivec3",
    "ivec4",
    "uvec2",
    "uvec3",
    "uvec4",
    "mat2",
    "mat3",
    "mat4",
    "mat2x2",
    "mat2x3",
    "mat2x4",
    "mat3x2",
    "mat3x3",
    "mat3x4",
    "mat4x2",
    "mat4x3",
    "mat4x4",
    "sampler2D",
    "samplerCube",
    "sampler3D",
    "sampler2DArray",
    "sampler2DShadow",
    "samplerCubeShadow",
    "sampler2DArrayShadow",
    "isampler2D",
    "isampler3D",
    "isamplerCube",
    "isampler2DArray",
    "usampler2D",
    "usampler3D",
    "usamplerCube",
    "usampler2DArray",
];

const QUALIFIER_WORDS: &[&str] = &[
    "const",
    "attribute",
    "uniform",
    "varying",
    "in",
    "out",
    "inout",
    "centroid",
    "flat",
    "smooth",
    "invariant",
    "highp",
    "mediump",
    "lowp",
    "layout",
];

#[derive(Clone, Debug)]
pub(crate) enum TypeSpec {
    Named(String),
    Struct(Box<StructDef>),
}

#[derive(Clone, Debug)]
pub(crate) struct StructDef {
    pub(crate) name: Option<String>,
    pub(crate) fields: Vec<(TypeSpec, Option<Expr>, String, Option<Expr>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Storage {
    #[default]
    Local,
    Const,
    Attribute,
    Uniform,
    Varying,
    In,
    Out,
    InOut,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Qualifiers {
    pub(crate) storage: Storage,
    pub(crate) flat: bool,
    pub(crate) invariant: bool,
    pub(crate) location: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct Expr {
    pub(crate) kind: ExprKind,
    pub(crate) line: u32,
}

#[derive(Clone, Debug)]
pub(crate) enum ExprKind {
    Ident(String),
    Int(u32, bool),
    Float(f32),
    Bool(bool),
    Unary(&'static str, Box<Expr>),
    Postfix(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Assign(&'static str, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    // a function, or a constructor when the callee is a type, with an array size for arrays
    Call(String, Option<Option<Box<Expr>>>, Vec<Expr>),
    Field(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Length(Box<Expr>),
    Comma(Box<Expr>, Box<Expr>),
}

#[derive(Clone, Debug)]
pub(crate) struct Variable {
    pub(crate) name: String,
    // an array size; an unsized array when the size is None but the brackets were there
    pub(crate) array: Option<Option<Expr>>,
    pub(crate) init: Option<Expr>,
    pub(crate) line: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct Declaration {
    pub(crate) qualifiers: Qualifiers,
    pub(crate) ty: TypeSpec,
    pub(crate) type_array: Option<Option<Expr>>,
    pub(crate) variables: Vec<Variable>,
}

#[derive(Clone, Debug)]
pub(crate) enum Stmt {
    Declare(Declaration),
    Expr(Expr),
    Block(Vec<Stmt>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    For(Option<Box<Stmt>>, Option<Expr>, Option<Expr>, Box<Stmt>),
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    Switch(Expr, Vec<SwitchItem>),
    Break,
    Continue,
    Return(Option<Expr>, u32),
    Discard,
    Empty,
}

#[derive(Clone, Debug)]
pub(crate) enum SwitchItem {
    Case(Expr),
    Default,
    Stmt(Stmt),
}

#[derive(Clone, Debug)]
pub(crate) struct Parameter {
    pub(crate) storage: Storage,
    pub(crate) constant: bool,
    pub(crate) ty: TypeSpec,
    pub(crate) name: Option<String>,
    pub(crate) array: Option<Expr>,
}

#[derive(Clone, Debug)]
pub(crate) struct Function {
    pub(crate) returns: TypeSpec,
    pub(crate) name: String,
    pub(crate) parameters: Vec<Parameter>,
    pub(crate) body: Option<Vec<Stmt>>,
    pub(crate) line: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct Block {
    pub(crate) storage: Storage,
    pub(crate) name: String,
    pub(crate) members: Vec<(TypeSpec, String, Option<Expr>)>,
    pub(crate) instance: Option<(String, Option<Option<Expr>>)>,
}

#[derive(Clone, Debug)]
pub(crate) enum External {
    Function(Function),
    Declare(Declaration),
    Block(Block),
}

pub(crate) struct Parser {
    tokens: Vec<Token>,
    at: usize,
    structs: HashSet<String>,
}

type Parsed<T> = Result<T, String>;

impl Parser {
    pub(crate) fn new(tokens: Vec<Token>) -> Self {
        Parser {
            tokens,
            at: 0,
            structs: HashSet::default(),
        }
    }

    fn line(&self) -> u32 {
        self.tokens
            .get(self.at)
            .or(self.tokens.last())
            .map_or(1, |token| token.line)
    }

    fn fail<T>(&self, message: &str) -> Parsed<T> {
        Err(format!("{}: {message}", self.line()))
    }

    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.at).map(|token| &token.tok)
    }

    fn peek_at(&self, offset: usize) -> Option<&Tok> {
        self.tokens.get(self.at + offset).map(|token| &token.tok)
    }

    fn is_punct(&self, punct: &str) -> bool {
        matches!(self.peek(), Some(Tok::Punct(found)) if *found == punct)
    }

    fn eat(&mut self, punct: &str) -> bool {
        if self.is_punct(punct) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, punct: &str) -> Parsed<()> {
        if self.eat(punct) {
            Ok(())
        } else {
            self.fail(&format!("expected '{punct}'"))
        }
    }

    fn word(&self) -> Option<&str> {
        match self.peek() {
            Some(Tok::Ident(word)) => Some(word.as_str()),
            _ => None,
        }
    }

    fn identifier(&mut self) -> Parsed<String> {
        match self.peek() {
            Some(Tok::Ident(word)) if !TYPE_WORDS.contains(&word.as_str()) => {
                let word = word.clone();
                self.at += 1;
                Ok(word)
            }
            _ => self.fail("expected an identifier"),
        }
    }

    fn is_type_word(&self, offset: usize) -> bool {
        matches!(self.peek_at(offset), Some(Tok::Ident(word)) if TYPE_WORDS.contains(&word.as_str()) || self.structs.contains(word) || word == "struct")
    }

    fn is_qualifier(&self) -> bool {
        matches!(self.word(), Some(word) if QUALIFIER_WORDS.contains(&word) || word == "precision")
    }

    pub(crate) fn translation_unit(&mut self) -> Parsed<Vec<External>> {
        let mut externals = Vec::new();
        while self.peek().is_some() {
            if self.eat(";") {
                continue;
            }
            externals.push(self.external()?);
        }
        Ok(externals)
    }

    fn qualifiers(&mut self) -> Parsed<Qualifiers> {
        let mut qualifiers = Qualifiers::default();
        loop {
            let Some(word) = self.word().map(str::to_string) else {
                return Ok(qualifiers);
            };
            match word.as_str() {
                "const" => qualifiers.storage = Storage::Const,
                "attribute" => qualifiers.storage = Storage::Attribute,
                "uniform" => qualifiers.storage = Storage::Uniform,
                "varying" => qualifiers.storage = Storage::Varying,
                "in" => qualifiers.storage = Storage::In,
                "out" => qualifiers.storage = Storage::Out,
                "inout" => qualifiers.storage = Storage::InOut,
                "flat" => qualifiers.flat = true,
                "invariant" => qualifiers.invariant = true,
                "centroid" | "smooth" | "highp" | "mediump" | "lowp" => {}
                "layout" => {
                    self.at += 1;
                    self.expect("(")?;
                    loop {
                        let key = self.identifier()?;
                        if self.eat("=") {
                            let value = match self.peek() {
                                Some(Tok::Int(value, _)) => i64::from(*value),
                                _ => return self.fail("expected a layout value"),
                            };
                            self.at += 1;
                            if key == "location" {
                                qualifiers.location = Some(value);
                            }
                        }
                        if !self.eat(",") {
                            break;
                        }
                    }
                    self.expect(")")?;
                    continue;
                }
                _ => return Ok(qualifiers),
            }
            self.at += 1;
        }
    }

    fn type_spec(&mut self) -> Parsed<TypeSpec> {
        let Some(word) = self.word().map(str::to_string) else {
            return self.fail("expected a type");
        };
        if word == "struct" {
            self.at += 1;
            let name = match self.word() {
                Some(_) if !self.is_punct("{") => Some(self.identifier()?),
                _ => None,
            };
            self.expect("{")?;
            let mut fields = Vec::new();
            while !self.eat("}") {
                self.qualifiers()?;
                let ty = self.type_spec()?;
                let type_array = self
                    .array_suffix()?
                    .map(|size| {
                        size.ok_or_else(|| format!("{}: struct fields need a size", self.line()))
                    })
                    .transpose()?;
                loop {
                    let field = self.identifier()?;
                    let array = self
                        .array_suffix()?
                        .map(|size| {
                            size.ok_or_else(|| {
                                format!("{}: struct fields need a size", self.line())
                            })
                        })
                        .transpose()?;
                    fields.push((ty.clone(), type_array.clone(), field, array));
                    if !self.eat(",") {
                        break;
                    }
                }
                self.expect(";")?;
            }
            if fields.is_empty() {
                return self.fail("a struct needs fields");
            }
            if let Some(name) = &name {
                self.structs.insert(name.clone());
            }
            return Ok(TypeSpec::Struct(Box::new(StructDef { name, fields })));
        }
        if TYPE_WORDS.contains(&word.as_str()) || self.structs.contains(&word) {
            self.at += 1;
            return Ok(TypeSpec::Named(word));
        }
        self.fail(&format!("unknown type {word}"))
    }

    // [] or [size] after a type or name
    fn array_suffix(&mut self) -> Parsed<Option<Option<Expr>>> {
        if !self.eat("[") {
            return Ok(None);
        }
        if self.eat("]") {
            return Ok(Some(None));
        }
        let size = self.conditional()?;
        self.expect("]")?;
        Ok(Some(Some(size)))
    }

    fn external(&mut self) -> Parsed<External> {
        if self.word() == Some("precision") {
            self.at += 1;
            self.qualifiers()?;
            self.type_spec()?;
            self.expect(";")?;
            return Ok(External::Declare(Declaration {
                qualifiers: Qualifiers::default(),
                ty: TypeSpec::Named("void".into()),
                type_array: None,
                variables: Vec::new(),
            }));
        }
        let line = self.line();
        let qualifiers = self.qualifiers()?;
        // invariant gl_Position;
        if qualifiers.invariant
            && matches!(self.word(), Some(word) if !self.is_type_word(0) && word.starts_with("gl_"))
        {
            self.identifier()?;
            while self.eat(",") {
                self.identifier()?;
            }
            self.expect(";")?;
            return Ok(External::Declare(Declaration {
                qualifiers,
                ty: TypeSpec::Named("void".into()),
                type_array: None,
                variables: Vec::new(),
            }));
        }
        // an interface block: uniform Name { members } instance;
        if matches!(
            qualifiers.storage,
            Storage::Uniform | Storage::In | Storage::Out
        ) && self.word().is_some()
            && !self.is_type_word(0)
            && matches!(self.peek_at(1), Some(Tok::Punct("{")))
        {
            let name = self.identifier()?;
            self.expect("{")?;
            let mut members = Vec::new();
            while !self.eat("}") {
                self.qualifiers()?;
                let ty = self.type_spec()?;
                loop {
                    let member = self.identifier()?;
                    let array = self
                        .array_suffix()?
                        .map(|size| {
                            size.ok_or_else(|| {
                                format!("{}: block members need a size", self.line())
                            })
                        })
                        .transpose()?;
                    members.push((ty.clone(), member, array));
                    if !self.eat(",") {
                        break;
                    }
                }
                self.expect(";")?;
            }
            let instance = if self.word().is_some() {
                let instance = self.identifier()?;
                let array = self.array_suffix()?;
                Some((instance, array))
            } else {
                None
            };
            self.expect(";")?;
            return Ok(External::Block(Block {
                storage: qualifiers.storage,
                name,
                members,
                instance,
            }));
        }
        let ty = self.type_spec()?;
        let type_array = self.array_suffix()?;
        if self.eat(";") {
            return Ok(External::Declare(Declaration {
                qualifiers,
                ty,
                type_array,
                variables: Vec::new(),
            }));
        }
        let name = self.identifier()?;
        if self.is_punct("(") {
            return self.function(ty, name, line).map(External::Function);
        }
        self.declarators(qualifiers, ty, type_array, name)
            .map(External::Declare)
    }

    fn function(&mut self, returns: TypeSpec, name: String, line: u32) -> Parsed<Function> {
        self.expect("(")?;
        let mut parameters = Vec::new();
        if !self.eat(")") {
            loop {
                let mut storage = Storage::In;
                let mut constant = false;
                loop {
                    match self.word() {
                        Some("const") => constant = true,
                        Some("in") => storage = Storage::In,
                        Some("out") => storage = Storage::Out,
                        Some("inout") => storage = Storage::InOut,
                        Some("highp" | "mediump" | "lowp") => {}
                        _ => break,
                    }
                    self.at += 1;
                }
                let ty = self.type_spec()?;
                if matches!(&ty, TypeSpec::Named(name) if name == "void") && self.is_punct(")") {
                    self.at += 1;
                    break;
                }
                let mut array = self
                    .array_suffix()?
                    .map(|size| {
                        size.ok_or_else(|| format!("{}: parameters need a size", self.line()))
                    })
                    .transpose()?;
                let name = if self.word().is_some() && !self.is_type_word(0) {
                    Some(self.identifier()?)
                } else {
                    None
                };
                if name.is_some()
                    && let Some(size) = self.array_suffix()?
                {
                    array = Some(
                        size.ok_or_else(|| format!("{}: parameters need a size", self.line()))?,
                    );
                }
                parameters.push(Parameter {
                    storage,
                    constant,
                    ty,
                    name,
                    array,
                });
                if self.eat(")") {
                    break;
                }
                self.expect(",")?;
            }
        }
        let body = if self.eat(";") {
            None
        } else {
            Some(self.compound()?)
        };
        Ok(Function {
            returns,
            name,
            parameters,
            body,
            line,
        })
    }

    fn declarators(
        &mut self,
        qualifiers: Qualifiers,
        ty: TypeSpec,
        type_array: Option<Option<Expr>>,
        first: String,
    ) -> Parsed<Declaration> {
        let mut variables = Vec::new();
        let mut name = first;
        loop {
            let line = self.line();
            let array = self.array_suffix()?;
            let init = if self.eat("=") {
                Some(self.assignment()?)
            } else {
                None
            };
            variables.push(Variable {
                name,
                array,
                init,
                line,
            });
            if !self.eat(",") {
                break;
            }
            name = self.identifier()?;
        }
        self.expect(";")?;
        Ok(Declaration {
            qualifiers,
            ty,
            type_array,
            variables,
        })
    }

    fn compound(&mut self) -> Parsed<Vec<Stmt>> {
        self.expect("{")?;
        let mut statements = Vec::new();
        while !self.eat("}") {
            if self.peek().is_none() {
                return self.fail("unterminated block");
            }
            statements.push(self.statement()?);
        }
        Ok(statements)
    }

    // whether a statement here starts with a declaration rather than an expression
    fn starts_declaration(&self) -> bool {
        if self.is_qualifier() {
            return true;
        }
        if !self.is_type_word(0) {
            return false;
        }
        matches!(self.peek_at(1), Some(Tok::Ident(_)))
            || (matches!(self.peek_at(1), Some(Tok::Punct("[")))
                && !matches!(self.peek_at(3), Some(Tok::Punct("(")))
                && !matches!(self.peek_at(4), Some(Tok::Punct("("))))
            || matches!(self.peek(), Some(Tok::Ident(word)) if word == "struct")
    }

    fn statement(&mut self) -> Parsed<Stmt> {
        let line = self.line();
        if self.is_punct("{") {
            return Ok(Stmt::Block(self.compound()?));
        }
        if self.eat(";") {
            return Ok(Stmt::Empty);
        }
        match self.word() {
            Some("if") => {
                self.at += 1;
                self.expect("(")?;
                let condition = self.expression()?;
                self.expect(")")?;
                let then = Box::new(self.statement()?);
                let otherwise = if self.word() == Some("else") {
                    self.at += 1;
                    Some(Box::new(self.statement()?))
                } else {
                    None
                };
                return Ok(Stmt::If(condition, then, otherwise));
            }
            Some("for") => {
                self.at += 1;
                self.expect("(")?;
                let init = if self.eat(";") {
                    None
                } else if self.starts_declaration() {
                    Some(Box::new(self.declaration_statement()?))
                } else {
                    let expression = self.expression()?;
                    self.expect(";")?;
                    Some(Box::new(Stmt::Expr(expression)))
                };
                let condition = if self.is_punct(";") {
                    None
                } else {
                    Some(self.expression()?)
                };
                self.expect(";")?;
                let step = if self.is_punct(")") {
                    None
                } else {
                    Some(self.expression()?)
                };
                self.expect(")")?;
                let body = Box::new(self.statement()?);
                return Ok(Stmt::For(init, condition, step, body));
            }
            Some("while") => {
                self.at += 1;
                self.expect("(")?;
                let condition = self.expression()?;
                self.expect(")")?;
                return Ok(Stmt::While(condition, Box::new(self.statement()?)));
            }
            Some("do") => {
                self.at += 1;
                let body = Box::new(self.statement()?);
                if self.word() != Some("while") {
                    return self.fail("expected while");
                }
                self.at += 1;
                self.expect("(")?;
                let condition = self.expression()?;
                self.expect(")")?;
                self.expect(";")?;
                return Ok(Stmt::DoWhile(body, condition));
            }
            Some("switch") => {
                self.at += 1;
                self.expect("(")?;
                let selector = self.expression()?;
                self.expect(")")?;
                self.expect("{")?;
                let mut items = Vec::new();
                while !self.eat("}") {
                    match self.word() {
                        Some("case") => {
                            self.at += 1;
                            let value = self.expression()?;
                            self.expect(":")?;
                            items.push(SwitchItem::Case(value));
                        }
                        Some("default") => {
                            self.at += 1;
                            self.expect(":")?;
                            items.push(SwitchItem::Default);
                        }
                        _ => items.push(SwitchItem::Stmt(self.statement()?)),
                    }
                }
                return Ok(Stmt::Switch(selector, items));
            }
            Some("break") => {
                self.at += 1;
                self.expect(";")?;
                return Ok(Stmt::Break);
            }
            Some("continue") => {
                self.at += 1;
                self.expect(";")?;
                return Ok(Stmt::Continue);
            }
            Some("discard") => {
                self.at += 1;
                self.expect(";")?;
                return Ok(Stmt::Discard);
            }
            Some("return") => {
                self.at += 1;
                let value = if self.is_punct(";") {
                    None
                } else {
                    Some(self.expression()?)
                };
                self.expect(";")?;
                return Ok(Stmt::Return(value, line));
            }
            _ => {}
        }
        if self.starts_declaration() {
            return self.declaration_statement();
        }
        let expression = self.expression()?;
        self.expect(";")?;
        Ok(Stmt::Expr(expression))
    }

    fn declaration_statement(&mut self) -> Parsed<Stmt> {
        let qualifiers = self.qualifiers()?;
        let ty = self.type_spec()?;
        let type_array = self.array_suffix()?;
        if self.eat(";") {
            return Ok(Stmt::Declare(Declaration {
                qualifiers,
                ty,
                type_array,
                variables: Vec::new(),
            }));
        }
        let name = self.identifier()?;
        Ok(Stmt::Declare(
            self.declarators(qualifiers, ty, type_array, name)?,
        ))
    }

    pub(crate) fn expression(&mut self) -> Parsed<Expr> {
        let mut left = self.assignment()?;
        while self.is_punct(",") {
            let line = self.line();
            self.at += 1;
            let right = self.assignment()?;
            left = Expr {
                kind: ExprKind::Comma(Box::new(left), Box::new(right)),
                line,
            };
        }
        Ok(left)
    }

    fn assignment(&mut self) -> Parsed<Expr> {
        let left = self.conditional()?;
        if let Some(Tok::Punct(op)) = self.peek()
            && matches!(
                *op,
                "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "<<=" | ">>=" | "&=" | "^=" | "|="
            )
        {
            let op = *op;
            let line = self.line();
            self.at += 1;
            let right = self.assignment()?;
            return Ok(Expr {
                kind: ExprKind::Assign(op, Box::new(left), Box::new(right)),
                line,
            });
        }
        Ok(left)
    }

    fn conditional(&mut self) -> Parsed<Expr> {
        let condition = self.binary(0)?;
        if self.is_punct("?") {
            let line = self.line();
            self.at += 1;
            let then = self.expression()?;
            self.expect(":")?;
            let otherwise = self.assignment()?;
            return Ok(Expr {
                kind: ExprKind::Ternary(Box::new(condition), Box::new(then), Box::new(otherwise)),
                line,
            });
        }
        Ok(condition)
    }

    fn binary(&mut self, minimum: u8) -> Parsed<Expr> {
        let mut left = self.unary()?;
        loop {
            let Some(Tok::Punct(op)) = self.peek() else {
                return Ok(left);
            };
            let precedence = match *op {
                "||" => 1,
                "^^" => 2,
                "&&" => 3,
                "|" => 4,
                "^" => 5,
                "&" => 6,
                "==" | "!=" => 7,
                "<" | ">" | "<=" | ">=" => 8,
                "<<" | ">>" => 9,
                "+" | "-" => 10,
                "*" | "/" | "%" => 11,
                _ => return Ok(left),
            };
            if precedence < minimum {
                return Ok(left);
            }
            let op = *op;
            let line = self.line();
            self.at += 1;
            let right = self.binary(precedence + 1)?;
            left = Expr {
                kind: ExprKind::Binary(op, Box::new(left), Box::new(right)),
                line,
            };
        }
    }

    fn unary(&mut self) -> Parsed<Expr> {
        if let Some(Tok::Punct(op)) = self.peek()
            && matches!(*op, "+" | "-" | "!" | "~" | "++" | "--")
        {
            let op = *op;
            let line = self.line();
            self.at += 1;
            let operand = self.unary()?;
            return Ok(Expr {
                kind: ExprKind::Unary(op, Box::new(operand)),
                line,
            });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Parsed<Expr> {
        let mut value = self.primary()?;
        loop {
            let line = self.line();
            if self.eat("[") {
                let index = self.expression()?;
                self.expect("]")?;
                value = Expr {
                    kind: ExprKind::Index(Box::new(value), Box::new(index)),
                    line,
                };
            } else if self.eat(".") {
                let Some(Tok::Ident(field)) = self.peek().cloned() else {
                    return self.fail("expected a field name");
                };
                self.at += 1;
                if field == "length" && self.is_punct("(") {
                    self.at += 1;
                    self.expect(")")?;
                    value = Expr {
                        kind: ExprKind::Length(Box::new(value)),
                        line,
                    };
                } else {
                    value = Expr {
                        kind: ExprKind::Field(Box::new(value), field),
                        line,
                    };
                }
            } else if let Some(Tok::Punct(op)) = self.peek()
                && matches!(*op, "++" | "--")
            {
                let op = *op;
                self.at += 1;
                value = Expr {
                    kind: ExprKind::Postfix(op, Box::new(value)),
                    line,
                };
            } else {
                return Ok(value);
            }
        }
    }

    fn primary(&mut self) -> Parsed<Expr> {
        let line = self.line();
        let Some(tok) = self.peek().cloned() else {
            return self.fail("unexpected end of shader");
        };
        let kind = match tok {
            Tok::Int(value, unsigned) => {
                self.at += 1;
                ExprKind::Int(value, unsigned)
            }
            Tok::Float(value) => {
                self.at += 1;
                ExprKind::Float(value)
            }
            Tok::Bool(value) => {
                self.at += 1;
                ExprKind::Bool(value)
            }
            Tok::Punct("(") => {
                self.at += 1;
                let inner = self.expression()?;
                self.expect(")")?;
                return Ok(inner);
            }
            Tok::Ident(word) => {
                self.at += 1;
                let array = if self.is_punct("[")
                    && (TYPE_WORDS.contains(&word.as_str()) || self.structs.contains(&word))
                {
                    self.array_suffix()?
                } else {
                    None
                };
                if self.eat("(") {
                    let mut arguments = Vec::new();
                    if self.word() == Some("void")
                        && matches!(self.peek_at(1), Some(Tok::Punct(")")))
                    {
                        self.at += 1;
                    }
                    if !self.eat(")") {
                        loop {
                            arguments.push(self.assignment()?);
                            if self.eat(")") {
                                break;
                            }
                            self.expect(",")?;
                        }
                    }
                    ExprKind::Call(word, array.map(|size| size.map(Box::new)), arguments)
                } else {
                    if array.is_some() {
                        return self.fail("an array type needs a constructor call");
                    }
                    ExprKind::Ident(word)
                }
            }
            Tok::Punct(other) => return self.fail(&format!("unexpected '{other}'")),
        };
        Ok(Expr { kind, line })
    }
}
