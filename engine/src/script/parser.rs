// a recursive-descent parser for ECMAScript 2022 scripts and modules; tokens are scanned
// on demand into a buffer, so arrow functions are found by trying the parameter grammar
// and rewinding, and regular expressions and template continuations are rescanned in place

use super::ScriptError;
use super::ast::*;
use super::lexer::{Lexer, Token, TokenKind};
use std::rc::Rc;

const RESERVED: [&str; 37] = [
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

const STRICT_RESERVED: [&str; 8] = [
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
];

#[derive(Clone, Copy, Default)]
struct Context {
    in_function: bool,
    in_async: bool,
    in_generator: bool,
    in_class_field: bool,
    allow_super_call: bool,
    allow_super_member: bool,
    strict: bool,
    module: bool,
}

pub(super) struct Parser<'a> {
    lexer: Lexer<'a>,
    tokens: Vec<Token>,
    position: usize,
    previous_end: usize,
    context: Context,
    template_sites: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Precedence {
    Lowest,
    Coalesce,
    LogicalOr,
    LogicalAnd,
    BitOr,
    BitXor,
    BitAnd,
    Equality,
    Relational,
    Shift,
    Additive,
    Multiplicative,
    Exponent,
}

impl Precedence {
    fn rank(self) -> u8 {
        self as u8
    }
}

enum BinaryKind {
    Binary(BinaryOperator),
    Logical(LogicalOperator),
}

fn binary_operator(token: &TokenKind, allow_in: bool) -> Option<(Precedence, BinaryKind)> {
    use BinaryOperator::*;
    let (precedence, kind) = match token {
        TokenKind::Punct(punct) => match *punct {
            "??" => (
                Precedence::Coalesce,
                BinaryKind::Logical(LogicalOperator::Nullish),
            ),
            "||" => (
                Precedence::LogicalOr,
                BinaryKind::Logical(LogicalOperator::Or),
            ),
            "&&" => (
                Precedence::LogicalAnd,
                BinaryKind::Logical(LogicalOperator::And),
            ),
            "|" => (Precedence::BitOr, BinaryKind::Binary(BitOr)),
            "^" => (Precedence::BitXor, BinaryKind::Binary(BitXor)),
            "&" => (Precedence::BitAnd, BinaryKind::Binary(BitAnd)),
            "==" => (Precedence::Equality, BinaryKind::Binary(Equal)),
            "!=" => (Precedence::Equality, BinaryKind::Binary(NotEqual)),
            "===" => (Precedence::Equality, BinaryKind::Binary(StrictEqual)),
            "!==" => (Precedence::Equality, BinaryKind::Binary(StrictNotEqual)),
            "<" => (Precedence::Relational, BinaryKind::Binary(Less)),
            "<=" => (Precedence::Relational, BinaryKind::Binary(LessEqual)),
            ">" => (Precedence::Relational, BinaryKind::Binary(Greater)),
            ">=" => (Precedence::Relational, BinaryKind::Binary(GreaterEqual)),
            "<<" => (Precedence::Shift, BinaryKind::Binary(ShiftLeft)),
            ">>" => (Precedence::Shift, BinaryKind::Binary(ShiftRight)),
            ">>>" => (Precedence::Shift, BinaryKind::Binary(ShiftRightUnsigned)),
            "+" => (Precedence::Additive, BinaryKind::Binary(Add)),
            "-" => (Precedence::Additive, BinaryKind::Binary(Subtract)),
            "*" => (Precedence::Multiplicative, BinaryKind::Binary(Multiply)),
            "/" => (Precedence::Multiplicative, BinaryKind::Binary(Divide)),
            "%" => (Precedence::Multiplicative, BinaryKind::Binary(Remainder)),
            "**" => (Precedence::Exponent, BinaryKind::Binary(Exponent)),
            _ => return None,
        },
        TokenKind::Identifier(word) => match word.as_str() {
            "instanceof" => (Precedence::Relational, BinaryKind::Binary(InstanceOf)),
            "in" if allow_in => (Precedence::Relational, BinaryKind::Binary(In)),
            _ => return None,
        },
        _ => return None,
    };
    Some((precedence, kind))
}

fn assign_operator(punct: &str) -> Option<AssignOperator> {
    use BinaryOperator::*;
    Some(match punct {
        "=" => AssignOperator::Assign,
        "+=" => AssignOperator::Binary(Add),
        "-=" => AssignOperator::Binary(Subtract),
        "*=" => AssignOperator::Binary(Multiply),
        "/=" => AssignOperator::Binary(Divide),
        "%=" => AssignOperator::Binary(Remainder),
        "**=" => AssignOperator::Binary(Exponent),
        "<<=" => AssignOperator::Binary(ShiftLeft),
        ">>=" => AssignOperator::Binary(ShiftRight),
        ">>>=" => AssignOperator::Binary(ShiftRightUnsigned),
        "&=" => AssignOperator::Binary(BitAnd),
        "|=" => AssignOperator::Binary(BitOr),
        "^=" => AssignOperator::Binary(BitXor),
        "&&=" => AssignOperator::Logical(LogicalOperator::And),
        "||=" => AssignOperator::Logical(LogicalOperator::Or),
        "??=" => AssignOperator::Logical(LogicalOperator::Nullish),
        _ => return None,
    })
}

// property keys written as numbers use the number's canonical string
pub(super) fn number_key(value: f64) -> Name {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e21 {
        format!("{}", value as i128).into()
    } else if value.is_nan() {
        "NaN".into()
    } else if value.is_infinite() {
        if value > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else {
        format!("{value}").into()
    }
}

impl<'a> Parser<'a> {
    pub(super) fn new(source: &'a str) -> Self {
        Self {
            lexer: Lexer::new(source),
            tokens: Vec::new(),
            position: 0,
            previous_end: 0,
            context: Context::default(),
            template_sites: 0,
        }
    }

    pub(super) fn parse_script(mut self) -> Result<Script, ScriptError> {
        let (body, strict) = self.parse_body_with_directives(true)?;
        self.expect_eof()?;
        Ok(Script { body, strict })
    }

    pub(super) fn parse_module(mut self) -> Result<Module, ScriptError> {
        self.context.strict = true;
        self.context.module = true;
        self.context.in_async = true;
        let mut items = Vec::new();
        while !self.at_eof()? {
            if self.is_word("import")? && !matches!(self.peek_punct_at(1)?, Some("(" | ".")) {
                items.push(ModuleItem::Import(self.parse_import()?));
            } else if self.is_word("export")? {
                self.advance()?;
                items.extend(self.parse_export()?);
            } else {
                items.push(ModuleItem::Statement(self.parse_statement_list_item()?));
            }
        }
        Ok(Module { items })
    }

    // parses an expression on its own, as the Function constructor and template tests need
    pub(super) fn parse_standalone_expression(mut self) -> Result<Expr, ScriptError> {
        let expression = self.parse_expression(true)?;
        self.expect_eof()?;
        Ok(expression)
    }

    // ---- token access ----

    fn fill(&mut self, offset: usize) -> Result<(), ScriptError> {
        while self.tokens.len() <= self.position + offset {
            if self
                .tokens
                .last()
                .is_some_and(|token| token.kind == TokenKind::Eof)
            {
                let eof = self.tokens.last().expect("checked above").clone();
                self.tokens.push(eof);
                continue;
            }
            let token = self.lexer.next_token()?;
            self.tokens.push(token);
        }
        Ok(())
    }

    fn peek(&mut self) -> Result<&Token, ScriptError> {
        self.fill(0)?;
        Ok(&self.tokens[self.position])
    }

    fn peek_at(&mut self, offset: usize) -> Result<&Token, ScriptError> {
        self.fill(offset)?;
        Ok(&self.tokens[self.position + offset])
    }

    fn peek_kind(&mut self) -> Result<TokenKind, ScriptError> {
        Ok(self.peek()?.kind.clone())
    }

    fn peek_punct_at(&mut self, offset: usize) -> Result<Option<&'static str>, ScriptError> {
        Ok(match self.peek_at(offset)?.kind {
            TokenKind::Punct(punct) => Some(punct),
            _ => None,
        })
    }

    fn advance(&mut self) -> Result<Token, ScriptError> {
        self.fill(0)?;
        let token = self.tokens[self.position].clone();
        self.position += 1;
        self.previous_end = token.end;
        Ok(token)
    }

    fn at_eof(&mut self) -> Result<bool, ScriptError> {
        Ok(self.peek()?.kind == TokenKind::Eof)
    }

    fn offset(&mut self) -> Result<usize, ScriptError> {
        Ok(self.peek()?.start)
    }

    fn error_here(&mut self, message: impl Into<String>) -> ScriptError {
        let offset = self.peek().map(|token| token.start).unwrap_or(0);
        ScriptError {
            offset,
            message: message.into(),
        }
    }

    fn is_punct(&mut self, punct: &str) -> Result<bool, ScriptError> {
        Ok(matches!(self.peek()?.kind, TokenKind::Punct(found) if found == punct))
    }

    fn eat_punct(&mut self, punct: &str) -> Result<bool, ScriptError> {
        if self.is_punct(punct)? {
            self.advance()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expect_punct(&mut self, punct: &str) -> Result<Token, ScriptError> {
        if self.is_punct(punct)? {
            self.advance()
        } else {
            let found = self.describe_current()?;
            Err(self.error_here(format!("expected {punct} but found {found}")))
        }
    }

    fn describe_current(&mut self) -> Result<String, ScriptError> {
        Ok(match self.peek_kind()? {
            TokenKind::Eof => "end of input".to_string(),
            TokenKind::Identifier(name) | TokenKind::EscapedIdentifier(name) => name,
            TokenKind::PrivateName(name) => format!("#{name}"),
            TokenKind::Punct(punct) => punct.to_string(),
            TokenKind::Number(value) => value.to_string(),
            TokenKind::String(_) => "a string".to_string(),
            TokenKind::Template { .. } => "a template".to_string(),
            TokenKind::Regex { .. } => "a regular expression".to_string(),
        })
    }

    fn is_word(&mut self, word: &str) -> Result<bool, ScriptError> {
        Ok(matches!(&self.peek()?.kind, TokenKind::Identifier(found) if found == word))
    }

    fn is_word_at(&mut self, offset: usize, word: &str) -> Result<bool, ScriptError> {
        Ok(matches!(&self.peek_at(offset)?.kind, TokenKind::Identifier(found) if found == word))
    }

    fn eat_word(&mut self, word: &str) -> Result<bool, ScriptError> {
        if self.is_word(word)? {
            self.advance()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn expect_word(&mut self, word: &str) -> Result<(), ScriptError> {
        if self.eat_word(word)? {
            Ok(())
        } else {
            let found = self.describe_current()?;
            Err(self.error_here(format!("expected {word} but found {found}")))
        }
    }

    fn newline_before_current(&mut self) -> Result<bool, ScriptError> {
        Ok(self.peek()?.newline_before)
    }

    // automatic semicolon insertion: a newline, a closing brace or the end stands in for one
    fn consume_semicolon(&mut self) -> Result<(), ScriptError> {
        if self.eat_punct(";")? {
            return Ok(());
        }
        if self.is_punct("}")? || self.at_eof()? || self.newline_before_current()? {
            return Ok(());
        }
        let found = self.describe_current()?;
        Err(self.error_here(format!("expected ; but found {found}")))
    }

    fn expect_eof(&mut self) -> Result<(), ScriptError> {
        if self.at_eof()? {
            Ok(())
        } else {
            let found = self.describe_current()?;
            Err(self.error_here(format!("unexpected {found}")))
        }
    }

    // ---- names ----

    fn is_reserved(&self, name: &str) -> bool {
        RESERVED.contains(&name)
            && !(name == "yield" && !self.context.in_generator && !self.context.strict)
            || self.context.strict && STRICT_RESERVED.contains(&name)
            || name == "await" && (self.context.in_async || self.context.module)
    }

    fn is_binding_identifier(&mut self) -> Result<bool, ScriptError> {
        Ok(match self.peek_kind()? {
            TokenKind::Identifier(name) => !self.is_reserved(&name),
            TokenKind::EscapedIdentifier(_) => true,
            _ => false,
        })
    }

    fn parse_binding_identifier(&mut self) -> Result<Name, ScriptError> {
        match self.peek_kind()? {
            TokenKind::Identifier(name) if !self.is_reserved(&name) => {
                self.advance()?;
                Ok(name.into())
            }
            TokenKind::EscapedIdentifier(name) => {
                self.advance()?;
                Ok(name.into())
            }
            _ => {
                let found = self.describe_current()?;
                Err(self.error_here(format!("expected an identifier but found {found}")))
            }
        }
    }

    // any identifier name, keywords included, as property names allow
    fn parse_identifier_name(&mut self) -> Result<Name, ScriptError> {
        match self.peek_kind()? {
            TokenKind::Identifier(name) | TokenKind::EscapedIdentifier(name) => {
                self.advance()?;
                Ok(name.into())
            }
            _ => {
                let found = self.describe_current()?;
                Err(self.error_here(format!("expected a property name but found {found}")))
            }
        }
    }

    // ---- statements ----

    fn parse_body_with_directives(
        &mut self,
        until_eof: bool,
    ) -> Result<(Vec<Stmt>, bool), ScriptError> {
        let mut body = Vec::new();
        let mut strict = self.context.strict;
        let mut in_prologue = true;
        loop {
            if until_eof && self.at_eof()? || !until_eof && self.is_punct("}")? {
                break;
            }
            if in_prologue {
                if let TokenKind::String(value) = self.peek_kind()? {
                    let start = self.peek()?.start;
                    let end = self.peek()?.end;
                    let raw = &self.lexer.source()[start..end];
                    let next_ends = matches!(self.peek_punct_at(1)?, Some(";" | "}"))
                        || self.peek_at(1)?.newline_before
                        || self.peek_at(1)?.kind == TokenKind::Eof;
                    if next_ends {
                        if raw == "'use strict'" || raw == "\"use strict\"" {
                            strict = true;
                            self.context.strict = true;
                        }
                        let _ = value;
                    } else {
                        in_prologue = false;
                    }
                } else {
                    in_prologue = false;
                }
            }
            body.push(self.parse_statement_list_item()?);
        }
        Ok((body, strict))
    }

    fn parse_statement_list_item(&mut self) -> Result<Stmt, ScriptError> {
        if self.is_word("function")? {
            let start = self.offset()?;
            self.advance()?;
            return Ok(Stmt::Function(
                self.parse_function_declaration(start, false)?,
            ));
        }
        if self.is_word("async")?
            && self.is_word_at(1, "function")?
            && !self.peek_at(1)?.newline_before
        {
            let start = self.offset()?;
            self.advance()?;
            self.advance()?;
            return Ok(Stmt::Function(
                self.parse_function_declaration(start, true)?,
            ));
        }
        if self.is_word("class")? {
            let class = self.parse_class(true)?;
            return Ok(Stmt::Class(class));
        }
        if self.is_word("const")? || self.is_let_declaration()? {
            let kind = if self.eat_word("const")? {
                DeclarationKind::Const
            } else {
                self.advance()?;
                DeclarationKind::Let
            };
            let declarators = self.parse_declarators(kind, true)?;
            self.consume_semicolon()?;
            return Ok(Stmt::Declaration { kind, declarators });
        }
        self.parse_statement()
    }

    // "let" starts a declaration when a binding follows it, otherwise it is a name
    fn is_let_declaration(&mut self) -> Result<bool, ScriptError> {
        if !self.is_word("let")? {
            return Ok(false);
        }
        Ok(match self.peek_at(1)?.kind.clone() {
            TokenKind::Punct("[" | "{") => true,
            TokenKind::Identifier(name) => {
                !(RESERVED.contains(&name.as_str()) && name != "yield" && name != "let")
                    || name == "await"
            }
            TokenKind::EscapedIdentifier(_) => true,
            _ => false,
        })
    }

    fn parse_declarators(
        &mut self,
        kind: DeclarationKind,
        allow_in: bool,
    ) -> Result<Vec<Declarator>, ScriptError> {
        let mut declarators = Vec::new();
        loop {
            let target = self.parse_binding_target()?;
            let init = if self.eat_punct("=")? {
                Some(self.parse_assignment(allow_in)?)
            } else {
                if kind == DeclarationKind::Const && allow_in {
                    return Err(self.error_here("a const declaration needs an initializer"));
                }
                if !matches!(target, Pattern::Identifier(_)) && allow_in {
                    return Err(self.error_here("a destructuring declaration needs an initializer"));
                }
                None
            };
            declarators.push(Declarator { target, init });
            if !self.eat_punct(",")? {
                return Ok(declarators);
            }
        }
    }

    fn parse_statement(&mut self) -> Result<Stmt, ScriptError> {
        let kind = self.peek_kind()?;
        match kind {
            TokenKind::Punct("{") => {
                self.advance()?;
                let body = self.parse_block_body()?;
                Ok(Stmt::Block(body))
            }
            TokenKind::Punct(";") => {
                self.advance()?;
                Ok(Stmt::Empty)
            }
            TokenKind::Identifier(word) => match word.as_str() {
                "var" => {
                    self.advance()?;
                    let declarators = self.parse_declarators(DeclarationKind::Var, true)?;
                    self.consume_semicolon()?;
                    Ok(Stmt::Declaration {
                        kind: DeclarationKind::Var,
                        declarators,
                    })
                }
                "if" => self.parse_if(),
                "for" => self.parse_for(),
                "while" => {
                    self.advance()?;
                    self.expect_punct("(")?;
                    let test = self.parse_expression(true)?;
                    self.expect_punct(")")?;
                    let body = Box::new(self.parse_statement()?);
                    Ok(Stmt::While { test, body })
                }
                "do" => {
                    self.advance()?;
                    let body = Box::new(self.parse_statement()?);
                    self.expect_word("while")?;
                    self.expect_punct("(")?;
                    let test = self.parse_expression(true)?;
                    self.expect_punct(")")?;
                    // the semicolon after do-while is always optional
                    self.eat_punct(";")?;
                    Ok(Stmt::DoWhile { body, test })
                }
                "switch" => self.parse_switch(),
                "break" | "continue" => {
                    self.advance()?;
                    let label =
                        if !self.newline_before_current()? && self.is_binding_identifier()? {
                            Some(self.parse_binding_identifier()?)
                        } else {
                            None
                        };
                    self.consume_semicolon()?;
                    Ok(if word == "break" {
                        Stmt::Break(label)
                    } else {
                        Stmt::Continue(label)
                    })
                }
                "return" => {
                    if !self.context.in_function {
                        return Err(self.error_here("return is only valid inside a function"));
                    }
                    self.advance()?;
                    let argument = if self.is_punct(";")?
                        || self.is_punct("}")?
                        || self.at_eof()?
                        || self.newline_before_current()?
                    {
                        None
                    } else {
                        Some(self.parse_expression(true)?)
                    };
                    self.consume_semicolon()?;
                    Ok(Stmt::Return(argument))
                }
                "throw" => {
                    self.advance()?;
                    if self.newline_before_current()? {
                        return Err(self.error_here("a newline cannot follow throw"));
                    }
                    let argument = self.parse_expression(true)?;
                    self.consume_semicolon()?;
                    Ok(Stmt::Throw(argument))
                }
                "try" => self.parse_try(),
                "debugger" => {
                    self.advance()?;
                    self.consume_semicolon()?;
                    Ok(Stmt::Debugger)
                }
                "with" => Err(self.error_here("with statements are not supported")),
                "function" => {
                    // sloppy code may declare a function as an if body
                    let start = self.offset()?;
                    self.advance()?;
                    Ok(Stmt::Function(
                        self.parse_function_declaration(start, false)?,
                    ))
                }
                "class" | "const" => {
                    Err(self.error_here(format!("a {word} declaration cannot appear here")))
                }
                _ if self.is_let_declaration()? => {
                    Err(self.error_here("a let declaration cannot appear here"))
                }
                _ if matches!(self.peek_punct_at(1)?, Some(":")) && !self.is_reserved(&word) => {
                    let label: Name = word.into();
                    self.advance()?;
                    self.advance()?;
                    let body = Box::new(self.parse_statement()?);
                    Ok(Stmt::Labeled { label, body })
                }
                _ => self.parse_expression_statement(),
            },
            _ => self.parse_expression_statement(),
        }
    }

    fn parse_expression_statement(&mut self) -> Result<Stmt, ScriptError> {
        let expression = self.parse_expression(true)?;
        self.consume_semicolon()?;
        Ok(Stmt::Expression(expression))
    }

    fn parse_block_body(&mut self) -> Result<Vec<Stmt>, ScriptError> {
        let mut body = Vec::new();
        while !self.is_punct("}")? {
            if self.at_eof()? {
                return Err(self.error_here("expected } before the end of input"));
            }
            body.push(self.parse_statement_list_item()?);
        }
        self.advance()?;
        Ok(body)
    }

    fn parse_if(&mut self) -> Result<Stmt, ScriptError> {
        self.advance()?;
        self.expect_punct("(")?;
        let test = self.parse_expression(true)?;
        self.expect_punct(")")?;
        let consequent = Box::new(self.parse_statement()?);
        let alternate = if self.eat_word("else")? {
            Some(Box::new(self.parse_statement()?))
        } else {
            None
        };
        Ok(Stmt::If {
            test,
            consequent,
            alternate,
        })
    }

    fn parse_for(&mut self) -> Result<Stmt, ScriptError> {
        self.advance()?;
        let is_await = if self.is_word("await")? && self.context.in_async {
            self.advance()?;
            true
        } else {
            false
        };
        self.expect_punct("(")?;
        let init = if self.is_punct(";")? {
            None
        } else if self.is_word("var")? || self.is_word("const")? || self.is_let_declaration()? {
            let kind = match self.advance()?.kind {
                TokenKind::Identifier(word) if word == "var" => DeclarationKind::Var,
                TokenKind::Identifier(word) if word == "const" => DeclarationKind::Const,
                _ => DeclarationKind::Let,
            };
            let target = self.parse_binding_target()?;
            if self.is_word("of")? || self.is_word("in")? {
                let left = ForHead::Declaration { kind, target };
                return self.parse_for_in_of(left, is_await);
            }
            let mut declarators = vec![Declarator {
                target,
                init: if self.eat_punct("=")? {
                    Some(self.parse_assignment(false)?)
                } else {
                    None
                },
            }];
            if self.eat_punct(",")? {
                declarators.extend(self.parse_declarators(kind, false)?);
            }
            Some(ForInit::Declaration { kind, declarators })
        } else {
            let start = self.offset()?;
            let expression = self.parse_expression(false)?;
            if self.is_word("of")? || self.is_word("in")? {
                let target = self.to_pattern(expression, start)?;
                return self.parse_for_in_of(ForHead::Target(target), is_await);
            }
            Some(ForInit::Expression(expression))
        };
        self.expect_punct(";")?;
        let test = if self.is_punct(";")? {
            None
        } else {
            Some(self.parse_expression(true)?)
        };
        self.expect_punct(";")?;
        let update = if self.is_punct(")")? {
            None
        } else {
            Some(self.parse_expression(true)?)
        };
        self.expect_punct(")")?;
        let body = Box::new(self.parse_statement()?);
        Ok(Stmt::For {
            init,
            test,
            update,
            body,
        })
    }

    fn parse_for_in_of(&mut self, left: ForHead, is_await: bool) -> Result<Stmt, ScriptError> {
        if self.eat_word("of")? {
            let right = self.parse_assignment(true)?;
            self.expect_punct(")")?;
            let body = Box::new(self.parse_statement()?);
            Ok(Stmt::ForOf {
                left,
                right,
                body,
                is_await,
            })
        } else {
            self.expect_word("in")?;
            let right = self.parse_expression(true)?;
            self.expect_punct(")")?;
            let body = Box::new(self.parse_statement()?);
            Ok(Stmt::ForIn { left, right, body })
        }
    }

    fn parse_switch(&mut self) -> Result<Stmt, ScriptError> {
        self.advance()?;
        self.expect_punct("(")?;
        let discriminant = self.parse_expression(true)?;
        self.expect_punct(")")?;
        self.expect_punct("{")?;
        let mut cases = Vec::new();
        let mut seen_default = false;
        while !self.eat_punct("}")? {
            let test = if self.eat_word("case")? {
                Some(self.parse_expression(true)?)
            } else if self.eat_word("default")? {
                if seen_default {
                    return Err(self.error_here("a switch has more than one default"));
                }
                seen_default = true;
                None
            } else {
                return Err(self.error_here("expected case or default"));
            };
            self.expect_punct(":")?;
            let mut body = Vec::new();
            while !self.is_word("case")? && !self.is_word("default")? && !self.is_punct("}")? {
                if self.at_eof()? {
                    return Err(self.error_here("unterminated switch"));
                }
                body.push(self.parse_statement_list_item()?);
            }
            cases.push(SwitchCase { test, body });
        }
        Ok(Stmt::Switch {
            discriminant,
            cases,
        })
    }

    fn parse_try(&mut self) -> Result<Stmt, ScriptError> {
        self.advance()?;
        self.expect_punct("{")?;
        let block = self.parse_block_body()?;
        let handler = if self.eat_word("catch")? {
            let param = if self.eat_punct("(")? {
                let param = self.parse_binding_target()?;
                self.expect_punct(")")?;
                Some(param)
            } else {
                None
            };
            self.expect_punct("{")?;
            let body = self.parse_block_body()?;
            Some(CatchClause { param, body })
        } else {
            None
        };
        let finalizer = if self.eat_word("finally")? {
            self.expect_punct("{")?;
            Some(self.parse_block_body()?)
        } else {
            None
        };
        if handler.is_none() && finalizer.is_none() {
            return Err(self.error_here("try needs a catch or finally clause"));
        }
        Ok(Stmt::Try {
            block,
            handler,
            finalizer,
        })
    }

    // ---- patterns ----

    fn parse_binding_target(&mut self) -> Result<Pattern, ScriptError> {
        if self.eat_punct("[")? {
            let mut elements = Vec::new();
            let mut rest = None;
            loop {
                if self.eat_punct("]")? {
                    break;
                }
                if self.eat_punct(",")? {
                    elements.push(None);
                    continue;
                }
                if self.eat_punct("...")? {
                    rest = Some(Box::new(self.parse_binding_target()?));
                    self.expect_punct("]")?;
                    break;
                }
                elements.push(Some(self.parse_binding_element()?));
                if !self.is_punct("]")? {
                    self.expect_punct(",")?;
                }
            }
            return Ok(Pattern::Array { elements, rest });
        }
        if self.eat_punct("{")? {
            let mut properties = Vec::new();
            let mut rest = None;
            while !self.eat_punct("}")? {
                if self.eat_punct("...")? {
                    rest = Some(Box::new(Pattern::Identifier(
                        self.parse_binding_identifier()?,
                    )));
                    self.expect_punct("}")?;
                    break;
                }
                let shorthand = matches!(
                    self.peek_kind()?,
                    TokenKind::Identifier(_) | TokenKind::EscapedIdentifier(_)
                ) && !matches!(self.peek_punct_at(1)?, Some(":"));
                if shorthand {
                    let name = self.parse_binding_identifier()?;
                    let mut value = Pattern::Identifier(name.clone());
                    if self.eat_punct("=")? {
                        value = Pattern::Default {
                            target: Box::new(value),
                            default: Box::new(self.parse_assignment(true)?),
                        };
                    }
                    properties.push((PropertyKey::Static(name), value));
                } else {
                    let key = self.parse_property_key(false)?;
                    self.expect_punct(":")?;
                    properties.push((key, self.parse_binding_element()?));
                }
                if !self.is_punct("}")? {
                    self.expect_punct(",")?;
                }
            }
            return Ok(Pattern::Object { properties, rest });
        }
        Ok(Pattern::Identifier(self.parse_binding_identifier()?))
    }

    fn parse_binding_element(&mut self) -> Result<Pattern, ScriptError> {
        let target = self.parse_binding_target()?;
        if self.eat_punct("=")? {
            Ok(Pattern::Default {
                target: Box::new(target),
                default: Box::new(self.parse_assignment(true)?),
            })
        } else {
            Ok(target)
        }
    }

    // reinterprets an expression as an assignment target, for destructuring assignment
    fn to_pattern(&self, expression: Expr, offset: usize) -> Result<Pattern, ScriptError> {
        let invalid = || ScriptError {
            offset,
            message: "invalid assignment target".to_string(),
        };
        Ok(match expression {
            Expr::Identifier(name) => {
                if self.context.strict && (&*name == "eval" || &*name == "arguments") {
                    return Err(invalid());
                }
                Pattern::Identifier(name)
            }
            Expr::Member {
                optional: false, ..
            }
            | Expr::Index {
                optional: false, ..
            }
            | Expr::PrivateMember {
                optional: false, ..
            }
            | Expr::SuperMember(_) => Pattern::Expression(Box::new(expression)),
            Expr::Assign {
                operator: AssignOperator::Assign,
                target,
                value,
            } => Pattern::Default {
                target,
                default: value,
            },
            Expr::Array(elements) => {
                let mut patterns = Vec::new();
                let mut rest = None;
                let count = elements.len();
                for (index, element) in elements.into_iter().enumerate() {
                    match element {
                        ArrayElement::Hole => patterns.push(None),
                        ArrayElement::Item(item) => {
                            patterns.push(Some(self.to_pattern(item, offset)?))
                        }
                        ArrayElement::Spread(item) if index + 1 == count => {
                            rest = Some(Box::new(self.to_pattern(item, offset)?));
                        }
                        ArrayElement::Spread(_) => return Err(invalid()),
                    }
                }
                Pattern::Array {
                    elements: patterns,
                    rest,
                }
            }
            Expr::Object(properties) => {
                let mut patterns = Vec::new();
                let mut rest = None;
                let count = properties.len();
                for (index, property) in properties.into_iter().enumerate() {
                    match property {
                        ObjectProperty::Value(key, value) => {
                            patterns.push((key, self.to_pattern(value, offset)?));
                        }
                        ObjectProperty::Prototype(value) => {
                            patterns.push((
                                PropertyKey::Static("__proto__".into()),
                                self.to_pattern(value, offset)?,
                            ));
                        }
                        ObjectProperty::Spread(value) if index + 1 == count => {
                            rest = Some(Box::new(self.to_pattern(value, offset)?));
                        }
                        _ => return Err(invalid()),
                    }
                }
                Pattern::Object {
                    properties: patterns,
                    rest,
                }
            }
            _ => return Err(invalid()),
        })
    }

    // ---- expressions ----

    pub(super) fn parse_expression(&mut self, allow_in: bool) -> Result<Expr, ScriptError> {
        let first = self.parse_assignment(allow_in)?;
        if !self.is_punct(",")? {
            return Ok(first);
        }
        let mut expressions = vec![first];
        while self.eat_punct(",")? {
            expressions.push(self.parse_assignment(allow_in)?);
        }
        Ok(Expr::Sequence(expressions))
    }

    fn parse_assignment(&mut self, allow_in: bool) -> Result<Expr, ScriptError> {
        if let Some(arrow) = self.try_parse_arrow(allow_in)? {
            return Ok(arrow);
        }
        if self.context.in_generator && self.is_word("yield")? {
            return self.parse_yield(allow_in);
        }
        let start = self.offset()?;
        let left = self.parse_conditional(allow_in)?;
        let operator = match self.peek_kind()? {
            TokenKind::Punct(punct) => assign_operator(punct),
            _ => None,
        };
        let Some(operator) = operator else {
            return Ok(left);
        };
        let target = if operator == AssignOperator::Assign {
            self.to_pattern(left, start)?
        } else {
            match left {
                Expr::Identifier(_)
                | Expr::Member {
                    optional: false, ..
                }
                | Expr::Index {
                    optional: false, ..
                }
                | Expr::PrivateMember {
                    optional: false, ..
                }
                | Expr::SuperMember(_) => self.to_pattern(left, start)?,
                _ => {
                    return Err(ScriptError {
                        offset: start,
                        message: "invalid compound assignment target".to_string(),
                    });
                }
            }
        };
        self.advance()?;
        let value = self.parse_assignment(allow_in)?;
        Ok(Expr::Assign {
            operator,
            target: Box::new(target),
            value: Box::new(value),
        })
    }

    fn parse_yield(&mut self, allow_in: bool) -> Result<Expr, ScriptError> {
        if self.context.in_class_field {
            return Err(self.error_here("yield is not valid in a class field"));
        }
        self.advance()?;
        if self.newline_before_current()? {
            return Ok(Expr::Yield {
                argument: None,
                delegate: false,
            });
        }
        let delegate = self.eat_punct("*")?;
        let ends = matches!(
            self.peek_kind()?,
            TokenKind::Punct(")" | "]" | "}" | "," | ";" | ":") | TokenKind::Eof
        ) || self.is_word("in")?;
        let argument = if ends && !delegate {
            None
        } else {
            Some(Box::new(self.parse_assignment(allow_in)?))
        };
        Ok(Expr::Yield { argument, delegate })
    }

    // tries the arrow parameter grammar and rewinds when no arrow follows
    fn try_parse_arrow(&mut self, allow_in: bool) -> Result<Option<Expr>, ScriptError> {
        let start = self.offset()?;
        let mut is_async = false;
        let mut lookahead = 0;
        if self.is_word("async")?
            && !self.peek_at(1)?.newline_before
            && matches!(
                self.peek_at(1)?.kind,
                TokenKind::Punct("(") | TokenKind::Identifier(_) | TokenKind::EscapedIdentifier(_)
            )
        {
            is_async = true;
            lookahead = 1;
        }
        // a single identifier parameter: x => ...
        let single = match self.peek_at(lookahead)?.kind.clone() {
            TokenKind::Identifier(name) => !self.is_reserved(&name),
            TokenKind::EscapedIdentifier(_) => true,
            _ => false,
        };
        if single
            && matches!(self.peek_at(lookahead + 1)?.kind, TokenKind::Punct("=>"))
            && !self.peek_at(lookahead + 1)?.newline_before
        {
            if is_async {
                self.advance()?;
            }
            let name = self.parse_binding_identifier()?;
            self.advance()?;
            return Ok(Some(self.parse_arrow_body(
                start,
                vec![Pattern::Identifier(name)],
                None,
                is_async,
                allow_in,
            )?));
        }
        if !matches!(self.peek_at(lookahead)?.kind, TokenKind::Punct("(")) {
            return Ok(None);
        }
        let saved_position = self.position;
        let saved_context = self.context;
        if is_async {
            self.advance()?;
            self.context.in_async = true;
        }
        let attempt = self.parse_formal_parameters();
        let arrow_follows = match &attempt {
            Ok(_) => self.is_punct("=>")? && !self.newline_before_current()?,
            Err(_) => false,
        };
        self.context = saved_context;
        if !arrow_follows {
            self.position = saved_position;
            return Ok(None);
        }
        let (params, rest) = attempt.expect("checked above");
        self.advance()?;
        Ok(Some(self.parse_arrow_body(
            start, params, rest, is_async, allow_in,
        )?))
    }

    fn parse_arrow_body(
        &mut self,
        start: usize,
        params: Vec<Pattern>,
        rest: Option<Pattern>,
        is_async: bool,
        allow_in: bool,
    ) -> Result<Expr, ScriptError> {
        let saved = self.context;
        self.context.in_async = is_async;
        self.context.in_generator = false;
        self.context.in_function = true;
        let body = if self.eat_punct("{")? {
            let (body, strict) = self.parse_body_with_directives(false)?;
            self.expect_punct("}")?;
            self.context.strict = strict;
            body
        } else {
            vec![Stmt::Return(Some(self.parse_assignment(allow_in)?))]
        };
        let strict = self.context.strict;
        self.context = saved;
        Ok(Expr::Function(Rc::new(FunctionNode {
            name: None,
            params,
            rest,
            body,
            kind: FunctionKind::Arrow,
            is_async,
            is_generator: false,
            strict,
            source: (start, self.previous_end),
        })))
    }

    fn parse_formal_parameters(&mut self) -> Result<(Vec<Pattern>, Option<Pattern>), ScriptError> {
        self.expect_punct("(")?;
        let mut params = Vec::new();
        let mut rest = None;
        while !self.eat_punct(")")? {
            if self.eat_punct("...")? {
                rest = Some(self.parse_binding_target()?);
                self.expect_punct(")")?;
                break;
            }
            params.push(self.parse_binding_element()?);
            if !self.is_punct(")")? {
                self.expect_punct(",")?;
            }
        }
        Ok((params, rest))
    }

    fn parse_conditional(&mut self, allow_in: bool) -> Result<Expr, ScriptError> {
        let test = self.parse_binary(Precedence::Lowest, allow_in)?;
        if !self.eat_punct("?")? {
            return Ok(test);
        }
        let consequent = self.parse_assignment(true)?;
        self.expect_punct(":")?;
        let alternate = self.parse_assignment(allow_in)?;
        Ok(Expr::Conditional {
            test: Box::new(test),
            consequent: Box::new(consequent),
            alternate: Box::new(alternate),
        })
    }

    fn parse_binary(&mut self, minimum: Precedence, allow_in: bool) -> Result<Expr, ScriptError> {
        let mut left = if let TokenKind::PrivateName(name) = self.peek_kind()? {
            // "#field in object" checks for a private name
            self.advance()?;
            self.expect_word("in")?;
            let object = self.parse_binary(Precedence::Relational, allow_in)?;
            Expr::PrivateIn {
                name: name.into(),
                object: Box::new(object),
            }
        } else {
            self.parse_unary()?
        };
        loop {
            let kind = self.peek_kind()?;
            let Some((precedence, operator)) = binary_operator(&kind, allow_in) else {
                return Ok(left);
            };
            if precedence.rank() <= minimum.rank() {
                // exponentiation is the one right-associative binary operator
                if !(precedence == Precedence::Exponent && minimum == Precedence::Exponent) {
                    return Ok(left);
                }
            }
            self.advance()?;
            let next_minimum = if precedence == Precedence::Exponent {
                Precedence::Multiplicative
            } else {
                precedence
            };
            let right = self.parse_binary(next_minimum, allow_in)?;
            left = match operator {
                BinaryKind::Binary(operator) => Expr::Binary {
                    operator,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                BinaryKind::Logical(operator) => Expr::Logical {
                    operator,
                    left: Box::new(left),
                    right: Box::new(right),
                },
            };
        }
    }

    fn parse_unary(&mut self) -> Result<Expr, ScriptError> {
        let start = self.offset()?;
        let operator = match self.peek_kind()? {
            TokenKind::Punct("!") => Some(UnaryOperator::Not),
            TokenKind::Punct("-") => Some(UnaryOperator::Negative),
            TokenKind::Punct("+") => Some(UnaryOperator::Positive),
            TokenKind::Punct("~") => Some(UnaryOperator::BitNot),
            TokenKind::Identifier(word) if word == "typeof" => Some(UnaryOperator::TypeOf),
            TokenKind::Identifier(word) if word == "void" => Some(UnaryOperator::Void),
            TokenKind::Identifier(word) if word == "delete" => Some(UnaryOperator::Delete),
            TokenKind::Identifier(word)
                if word == "await" && (self.context.in_async || self.context.module) =>
            {
                if self.context.in_class_field {
                    return Err(self.error_here("await is not valid in a class field"));
                }
                self.advance()?;
                let argument = self.parse_unary()?;
                return Ok(Expr::Await(Box::new(argument)));
            }
            TokenKind::Punct(punct @ ("++" | "--")) => {
                self.advance()?;
                let target = self.parse_unary()?;
                let target = self.check_update_target(target, start)?;
                return Ok(Expr::Update {
                    increment: punct == "++",
                    prefix: true,
                    target: Box::new(target),
                });
            }
            _ => None,
        };
        let Some(operator) = operator else {
            return self.parse_postfix();
        };
        self.advance()?;
        let argument = self.parse_unary()?;
        if operator == UnaryOperator::Delete
            && self.context.strict
            && matches!(argument, Expr::Identifier(_))
        {
            return Err(ScriptError {
                offset: start,
                message: "delete of an unqualified name in strict mode".to_string(),
            });
        }
        if self.is_punct("**")? {
            return Err(self.error_here("a unary expression cannot be the base of **"));
        }
        Ok(Expr::Unary {
            operator,
            argument: Box::new(argument),
        })
    }

    fn check_update_target(&self, target: Expr, offset: usize) -> Result<Expr, ScriptError> {
        match target {
            Expr::Identifier(_)
            | Expr::Member {
                optional: false, ..
            }
            | Expr::Index {
                optional: false, ..
            }
            | Expr::PrivateMember {
                optional: false, ..
            }
            | Expr::SuperMember(_) => Ok(target),
            _ => Err(ScriptError {
                offset,
                message: "invalid update target".to_string(),
            }),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, ScriptError> {
        let start = self.offset()?;
        let expression = self.parse_left_hand_side()?;
        if let TokenKind::Punct(punct @ ("++" | "--")) = self.peek_kind()?
            && !self.newline_before_current()?
        {
            self.advance()?;
            let target = self.check_update_target(expression, start)?;
            return Ok(Expr::Update {
                increment: punct == "++",
                prefix: false,
                target: Box::new(target),
            });
        }
        Ok(expression)
    }

    fn parse_arguments(&mut self) -> Result<Vec<Argument>, ScriptError> {
        self.expect_punct("(")?;
        let mut arguments = Vec::new();
        while !self.eat_punct(")")? {
            if self.eat_punct("...")? {
                arguments.push(Argument::Spread(self.parse_assignment(true)?));
            } else {
                arguments.push(Argument::Value(self.parse_assignment(true)?));
            }
            if !self.is_punct(")")? {
                self.expect_punct(",")?;
            }
        }
        Ok(arguments)
    }

    fn parse_left_hand_side(&mut self) -> Result<Expr, ScriptError> {
        let start = self.offset()?;
        let mut expression = if self.is_word("new")? {
            self.parse_new()?
        } else if self.is_word("super")? {
            self.parse_super()?
        } else if self.is_word("import")? {
            self.parse_import_expression()?
        } else {
            self.parse_primary()?
        };
        let mut in_chain = false;
        loop {
            match self.peek_kind()? {
                TokenKind::Punct(".") => {
                    self.advance()?;
                    expression = self.parse_member_name(expression, false)?;
                }
                TokenKind::Punct("?.") => {
                    self.advance()?;
                    in_chain = true;
                    expression = match self.peek_kind()? {
                        TokenKind::Punct("(") => Expr::Call {
                            callee: Box::new(expression),
                            arguments: self.parse_arguments()?,
                            optional: true,
                        },
                        TokenKind::Punct("[") => {
                            self.advance()?;
                            let index = self.parse_expression(true)?;
                            self.expect_punct("]")?;
                            Expr::Index {
                                object: Box::new(expression),
                                index: Box::new(index),
                                optional: true,
                            }
                        }
                        TokenKind::Template { .. } => {
                            return Err(self.error_here("an optional chain cannot be tagged"));
                        }
                        _ => self.parse_member_name(expression, true)?,
                    };
                }
                TokenKind::Punct("[") => {
                    self.advance()?;
                    let index = self.parse_expression(true)?;
                    self.expect_punct("]")?;
                    expression = Expr::Index {
                        object: Box::new(expression),
                        index: Box::new(index),
                        optional: false,
                    };
                }
                TokenKind::Punct("(") => {
                    expression = Expr::Call {
                        callee: Box::new(expression),
                        arguments: self.parse_arguments()?,
                        optional: false,
                    };
                }
                TokenKind::Template { .. } => {
                    if in_chain {
                        return Err(self.error_here("an optional chain cannot be tagged"));
                    }
                    expression = self.parse_tagged_template(expression)?;
                }
                _ => break,
            }
        }
        let _ = start;
        if in_chain {
            expression = Expr::OptionalChain(Box::new(expression));
        }
        Ok(expression)
    }

    fn parse_member_name(&mut self, object: Expr, optional: bool) -> Result<Expr, ScriptError> {
        if let TokenKind::PrivateName(name) = self.peek_kind()? {
            self.advance()?;
            return Ok(Expr::PrivateMember {
                object: Box::new(object),
                name: name.into(),
                optional,
            });
        }
        let property = self.parse_identifier_name()?;
        Ok(Expr::Member {
            object: Box::new(object),
            property,
            optional,
        })
    }

    fn parse_new(&mut self) -> Result<Expr, ScriptError> {
        self.advance()?;
        if self.eat_punct(".")? {
            self.expect_word("target")?;
            if !self.context.in_function {
                return Err(self.error_here("new.target is only valid inside a function"));
            }
            return Ok(Expr::NewTarget);
        }
        // the callee is a member expression: calls belong to the new, not the callee
        let mut callee = if self.is_word("new")? {
            self.parse_new()?
        } else if self.is_word("super")? {
            self.parse_super()?
        } else if self.is_word("import")? {
            return Err(self.error_here("import cannot be constructed"));
        } else {
            self.parse_primary()?
        };
        loop {
            match self.peek_kind()? {
                TokenKind::Punct(".") => {
                    self.advance()?;
                    callee = self.parse_member_name(callee, false)?;
                }
                TokenKind::Punct("[") => {
                    self.advance()?;
                    let index = self.parse_expression(true)?;
                    self.expect_punct("]")?;
                    callee = Expr::Index {
                        object: Box::new(callee),
                        index: Box::new(index),
                        optional: false,
                    };
                }
                TokenKind::Template { .. } => callee = self.parse_tagged_template(callee)?,
                TokenKind::Punct("?.") => {
                    return Err(self.error_here("an optional chain cannot follow new"));
                }
                _ => break,
            }
        }
        let arguments = if self.is_punct("(")? {
            self.parse_arguments()?
        } else {
            Vec::new()
        };
        Ok(Expr::New {
            callee: Box::new(callee),
            arguments,
        })
    }

    fn parse_super(&mut self) -> Result<Expr, ScriptError> {
        self.advance()?;
        match self.peek_kind()? {
            TokenKind::Punct("(") => {
                if !self.context.allow_super_call {
                    return Err(self.error_here("super() is only valid in a derived constructor"));
                }
                Ok(Expr::SuperCall(self.parse_arguments()?))
            }
            TokenKind::Punct(".") => {
                if !self.context.allow_super_member {
                    return Err(self.error_here("super is only valid inside a method"));
                }
                self.advance()?;
                Ok(Expr::SuperMember(PropertyKey::Static(
                    self.parse_identifier_name()?,
                )))
            }
            TokenKind::Punct("[") => {
                if !self.context.allow_super_member {
                    return Err(self.error_here("super is only valid inside a method"));
                }
                self.advance()?;
                let index = self.parse_expression(true)?;
                self.expect_punct("]")?;
                Ok(Expr::SuperMember(PropertyKey::Computed(Box::new(index))))
            }
            _ => Err(self.error_here("super must be called or followed by a property")),
        }
    }

    fn parse_import_expression(&mut self) -> Result<Expr, ScriptError> {
        self.advance()?;
        if self.eat_punct(".")? {
            self.expect_word("meta")?;
            if !self.context.module {
                return Err(self.error_here("import.meta is only valid in a module"));
            }
            return Ok(Expr::ImportMeta);
        }
        self.expect_punct("(")?;
        let specifier = Box::new(self.parse_assignment(true)?);
        let options = if self.eat_punct(",")? && !self.is_punct(")")? {
            let options = self.parse_assignment(true)?;
            self.eat_punct(",")?;
            Some(Box::new(options))
        } else {
            None
        };
        self.expect_punct(")")?;
        Ok(Expr::DynamicImport { specifier, options })
    }

    fn parse_tagged_template(&mut self, tag: Expr) -> Result<Expr, ScriptError> {
        let (cooked, raw, expressions) = self.parse_template_parts()?;
        let site = self.template_sites;
        self.template_sites += 1;
        Ok(Expr::TaggedTemplate {
            tag: Box::new(tag),
            cooked,
            raw,
            expressions,
            site,
        })
    }

    #[allow(clippy::type_complexity)]
    fn parse_template_parts(
        &mut self,
    ) -> Result<(Vec<Option<Name>>, Vec<Name>, Vec<Expr>), ScriptError> {
        let mut cooked = Vec::new();
        let mut raw = Vec::new();
        let mut expressions = Vec::new();
        let mut token = self.advance()?;
        loop {
            let TokenKind::Template {
                cooked: text,
                raw: raw_text,
                tail,
            } = token.kind
            else {
                return Err(ScriptError {
                    offset: token.start,
                    message: "expected a template".to_string(),
                });
            };
            cooked.push(text.map(Name::from));
            raw.push(raw_text.into());
            if tail {
                return Ok((cooked, raw, expressions));
            }
            expressions.push(self.parse_expression(true)?);
            if !self.is_punct("}")? {
                return Err(self.error_here("expected } to close a template substitution"));
            }
            // the brace continues the template, so it is rescanned as template text
            let start = self.peek()?.start;
            self.tokens.truncate(self.position);
            token = self.lexer.rescan_template_continuation(start)?;
            self.previous_end = token.end;
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, ScriptError> {
        let token = self.peek()?.clone();
        match token.kind {
            TokenKind::Number(value) => {
                self.advance()?;
                Ok(Expr::Number(value))
            }
            TokenKind::String(value) => {
                self.advance()?;
                Ok(Expr::String(value.into()))
            }
            TokenKind::Template { .. } => {
                let (cooked, _, expressions) = self.parse_template_parts()?;
                let quasis = cooked
                    .into_iter()
                    .map(|text| {
                        text.ok_or_else(|| ScriptError {
                            offset: token.start,
                            message: "invalid escape in template".to_string(),
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Expr::Template {
                    quasis,
                    expressions,
                })
            }
            TokenKind::Punct("/" | "/=") => {
                self.tokens.truncate(self.position);
                let regex = self.lexer.rescan_regex(token.start)?;
                self.tokens.push(Token {
                    newline_before: token.newline_before,
                    ..regex
                });
                let TokenKind::Regex { pattern, flags } = self.advance()?.kind else {
                    unreachable!("the rescan returns a regular expression");
                };
                Ok(Expr::Regex {
                    pattern: pattern.into(),
                    flags: flags.into(),
                })
            }
            TokenKind::Punct("(") => {
                self.advance()?;
                let expression = self.parse_expression(true)?;
                self.expect_punct(")")?;
                Ok(expression)
            }
            TokenKind::Punct("[") => self.parse_array_literal(),
            TokenKind::Punct("{") => self.parse_object_literal(),
            TokenKind::Identifier(word) => match word.as_str() {
                "this" => {
                    self.advance()?;
                    Ok(Expr::This)
                }
                "null" => {
                    self.advance()?;
                    Ok(Expr::Null)
                }
                "true" | "false" => {
                    self.advance()?;
                    Ok(Expr::Bool(word == "true"))
                }
                "function" => {
                    self.advance()?;
                    self.parse_function_expression(token.start, false)
                }
                "async" if self.is_word_at(1, "function")? && !self.peek_at(1)?.newline_before => {
                    self.advance()?;
                    self.advance()?;
                    self.parse_function_expression(token.start, true)
                }
                "class" => Ok(Expr::Class(self.parse_class(false)?)),
                _ if self.is_reserved(&word) => {
                    Err(self.error_here(format!("unexpected keyword {word}")))
                }
                _ => {
                    self.advance()?;
                    Ok(Expr::Identifier(word.into()))
                }
            },
            TokenKind::EscapedIdentifier(word) => {
                self.advance()?;
                Ok(Expr::Identifier(word.into()))
            }
            _ => {
                let found = self.describe_current()?;
                Err(self.error_here(format!("unexpected {found}")))
            }
        }
    }

    fn parse_array_literal(&mut self) -> Result<Expr, ScriptError> {
        self.advance()?;
        let mut elements = Vec::new();
        loop {
            if self.eat_punct("]")? {
                return Ok(Expr::Array(elements));
            }
            if self.eat_punct(",")? {
                elements.push(ArrayElement::Hole);
                continue;
            }
            if self.eat_punct("...")? {
                elements.push(ArrayElement::Spread(self.parse_assignment(true)?));
            } else {
                elements.push(ArrayElement::Item(self.parse_assignment(true)?));
            }
            if !self.is_punct("]")? {
                self.expect_punct(",")?;
            }
        }
    }

    fn parse_property_key(&mut self, allow_private: bool) -> Result<PropertyKey, ScriptError> {
        match self.peek_kind()? {
            TokenKind::Identifier(name) | TokenKind::EscapedIdentifier(name) => {
                self.advance()?;
                Ok(PropertyKey::Static(name.into()))
            }
            TokenKind::String(value) => {
                self.advance()?;
                Ok(PropertyKey::Static(value.into()))
            }
            TokenKind::Number(value) => {
                self.advance()?;
                Ok(PropertyKey::Static(number_key(value)))
            }
            TokenKind::PrivateName(name) if allow_private => {
                self.advance()?;
                Ok(PropertyKey::Private(name.into()))
            }
            TokenKind::Punct("[") => {
                self.advance()?;
                let key = self.parse_assignment(true)?;
                self.expect_punct("]")?;
                Ok(PropertyKey::Computed(Box::new(key)))
            }
            _ => {
                let found = self.describe_current()?;
                Err(self.error_here(format!("expected a property name but found {found}")))
            }
        }
    }

    // get, set, async and * prefixes, unless the word itself is the property name
    fn parse_method_prefix(&mut self) -> Result<(bool, bool, Option<MethodKind>), ScriptError> {
        let ends_name = |kind: &TokenKind| {
            matches!(
                kind,
                TokenKind::Punct("(" | ":" | "," | "}" | "=" | ";") | TokenKind::Eof
            )
        };
        let mut is_async = false;
        let mut accessor = None;
        if self.is_word("async")?
            && !ends_name(&self.peek_at(1)?.kind.clone())
            && !self.peek_at(1)?.newline_before
        {
            self.advance()?;
            is_async = true;
        } else if (self.is_word("get")? || self.is_word("set")?)
            && !ends_name(&self.peek_at(1)?.kind.clone())
        {
            let word = self.advance()?;
            accessor = Some(
                if matches!(word.kind, TokenKind::Identifier(ref name) if name == "get") {
                    MethodKind::Getter
                } else {
                    MethodKind::Setter
                },
            );
        }
        let is_generator = accessor.is_none() && self.eat_punct("*")?;
        Ok((is_async, is_generator, accessor))
    }

    fn parse_object_literal(&mut self) -> Result<Expr, ScriptError> {
        self.advance()?;
        let mut properties = Vec::new();
        while !self.eat_punct("}")? {
            if self.eat_punct("...")? {
                properties.push(ObjectProperty::Spread(self.parse_assignment(true)?));
            } else {
                let start = self.offset()?;
                let (is_async, is_generator, accessor) = self.parse_method_prefix()?;
                let shorthand_name = match self.peek_kind()? {
                    TokenKind::Identifier(name) | TokenKind::EscapedIdentifier(name)
                        if !is_async && !is_generator && accessor.is_none() =>
                    {
                        Some(name)
                    }
                    _ => None,
                };
                if let Some(name) = shorthand_name
                    && matches!(self.peek_punct_at(1)?, Some("," | "}" | "="))
                {
                    self.advance()?;
                    let name: Name = name.into();
                    let value = if self.eat_punct("=")? {
                        // only valid when the literal turns out to be a destructuring target
                        Expr::Assign {
                            operator: AssignOperator::Assign,
                            target: Box::new(Pattern::Identifier(name.clone())),
                            value: Box::new(self.parse_assignment(true)?),
                        }
                    } else {
                        Expr::Identifier(name.clone())
                    };
                    properties.push(ObjectProperty::Value(PropertyKey::Static(name), value));
                } else {
                    let key = self.parse_property_key(false)?;
                    if is_async || is_generator || accessor.is_some() || self.is_punct("(")? {
                        let kind = match accessor {
                            Some(MethodKind::Getter) => FunctionKind::Getter,
                            Some(MethodKind::Setter) => FunctionKind::Setter,
                            _ => FunctionKind::Method,
                        };
                        let function =
                            self.parse_method(start, &key, kind, is_async, is_generator, false)?;
                        properties.push(ObjectProperty::Method {
                            key,
                            kind: accessor.unwrap_or(MethodKind::Method),
                            function,
                        });
                    } else {
                        self.expect_punct(":")?;
                        let value = self.parse_assignment(true)?;
                        if matches!(&key, PropertyKey::Static(name) if &**name == "__proto__") {
                            properties.push(ObjectProperty::Prototype(value));
                        } else {
                            properties.push(ObjectProperty::Value(key, value));
                        }
                    }
                }
            }
            if !self.is_punct("}")? {
                self.expect_punct(",")?;
            }
        }
        Ok(Expr::Object(properties))
    }

    // ---- functions and classes ----

    fn key_name(key: &PropertyKey) -> Option<Name> {
        match key {
            PropertyKey::Static(name) => Some(name.clone()),
            PropertyKey::Private(name) => Some(format!("#{name}").into()),
            PropertyKey::Computed(_) => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn parse_function_rest(
        &mut self,
        start: usize,
        name: Option<Name>,
        kind: FunctionKind,
        is_async: bool,
        is_generator: bool,
        super_call: bool,
    ) -> Result<Rc<FunctionNode>, ScriptError> {
        let saved = self.context;
        self.context.in_async = is_async;
        self.context.in_generator = is_generator;
        self.context.in_function = true;
        self.context.in_class_field = false;
        if kind != FunctionKind::Normal {
            self.context.allow_super_member = true;
        } else {
            self.context.allow_super_member = false;
            self.context.allow_super_call = false;
        }
        self.context.allow_super_call = super_call;
        let (params, rest) = self.parse_formal_parameters()?;
        self.expect_punct("{")?;
        let (body, strict) = self.parse_body_with_directives(false)?;
        self.expect_punct("}")?;
        self.context = saved;
        Ok(Rc::new(FunctionNode {
            name,
            params,
            rest,
            body,
            kind,
            is_async,
            is_generator,
            strict,
            source: (start, self.previous_end),
        }))
    }

    fn parse_function_declaration(
        &mut self,
        start: usize,
        is_async: bool,
    ) -> Result<Rc<FunctionNode>, ScriptError> {
        let is_generator = self.eat_punct("*")?;
        let name = self.parse_binding_identifier()?;
        self.parse_function_rest(
            start,
            Some(name),
            FunctionKind::Normal,
            is_async,
            is_generator,
            false,
        )
    }

    fn parse_function_expression(
        &mut self,
        start: usize,
        is_async: bool,
    ) -> Result<Expr, ScriptError> {
        let is_generator = self.eat_punct("*")?;
        let name = if self.is_punct("(")? {
            None
        } else {
            // the name of a generator or async expression follows that function's rules
            let saved = self.context;
            self.context.in_generator = is_generator;
            self.context.in_async = is_async;
            let name = self.parse_binding_identifier();
            self.context = saved;
            Some(name?)
        };
        Ok(Expr::Function(self.parse_function_rest(
            start,
            name,
            FunctionKind::Normal,
            is_async,
            is_generator,
            false,
        )?))
    }

    fn parse_method(
        &mut self,
        start: usize,
        key: &PropertyKey,
        kind: FunctionKind,
        is_async: bool,
        is_generator: bool,
        super_call: bool,
    ) -> Result<Rc<FunctionNode>, ScriptError> {
        let name = Self::key_name(key).map(|name| match kind {
            FunctionKind::Getter => format!("get {name}").into(),
            FunctionKind::Setter => format!("set {name}").into(),
            _ => name,
        });
        let function =
            self.parse_function_rest(start, name, kind, is_async, is_generator, super_call)?;
        match kind {
            FunctionKind::Getter if !function.params.is_empty() || function.rest.is_some() => {
                Err(self.error_here("a getter takes no parameters"))
            }
            FunctionKind::Setter if function.params.len() != 1 || function.rest.is_some() => {
                Err(self.error_here("a setter takes exactly one parameter"))
            }
            _ => Ok(function),
        }
    }

    fn parse_class(&mut self, declaration: bool) -> Result<Rc<ClassNode>, ScriptError> {
        let start = self.offset()?;
        self.advance()?;
        let saved = self.context;
        self.context.strict = true;
        let name = if self.is_binding_identifier()? {
            Some(self.parse_binding_identifier()?)
        } else if declaration {
            return Err(self.error_here("a class declaration needs a name"));
        } else {
            None
        };
        let superclass = if self.eat_word("extends")? {
            Some(self.parse_left_hand_side()?)
        } else {
            None
        };
        self.expect_punct("{")?;
        let mut constructor = None;
        let mut members = Vec::new();
        while !self.eat_punct("}")? {
            if self.eat_punct(";")? {
                continue;
            }
            let member_start = self.offset()?;
            let is_static = self.is_word("static")?
                && !matches!(self.peek_punct_at(1)?, Some("(" | "=" | ";" | "}"))
                && !self.peek_at(1)?.newline_before
                || self.is_word("static")? && matches!(self.peek_punct_at(1)?, Some("{"));
            if is_static {
                self.advance()?;
                if self.is_punct("{")? {
                    let block = self.parse_class_body_function(member_start, true)?;
                    members.push(ClassMember {
                        key: PropertyKey::Static("static".into()),
                        is_static: true,
                        kind: ClassMemberKind::StaticBlock,
                        function: Some(block),
                    });
                    continue;
                }
            }
            let (is_async, is_generator, accessor) = self.parse_method_prefix()?;
            let key = self.parse_property_key(true)?;
            if self.is_punct("(")? {
                let is_constructor = !is_static
                    && matches!(&key, PropertyKey::Static(name) if &**name == "constructor");
                if is_constructor {
                    if is_async || is_generator || accessor.is_some() || constructor.is_some() {
                        return Err(self.error_here("invalid class constructor"));
                    }
                    let kind = if superclass.is_some() {
                        FunctionKind::DerivedConstructor
                    } else {
                        FunctionKind::ClassConstructor
                    };
                    let mut function = self.parse_method(
                        member_start,
                        &key,
                        kind,
                        false,
                        false,
                        superclass.is_some(),
                    )?;
                    if let Some(name) = &name {
                        Rc::make_mut(&mut function).name = Some(name.clone());
                    }
                    constructor = Some(function);
                    continue;
                }
                let kind = match accessor {
                    Some(MethodKind::Getter) => FunctionKind::Getter,
                    Some(MethodKind::Setter) => FunctionKind::Setter,
                    _ => FunctionKind::Method,
                };
                let function =
                    self.parse_method(member_start, &key, kind, is_async, is_generator, false)?;
                members.push(ClassMember {
                    key,
                    is_static,
                    kind: match accessor {
                        Some(MethodKind::Getter) => ClassMemberKind::Getter,
                        Some(MethodKind::Setter) => ClassMemberKind::Setter,
                        _ => ClassMemberKind::Method,
                    },
                    function: Some(function),
                });
                continue;
            }
            if is_async || is_generator || accessor.is_some() {
                return Err(self.error_here("expected a method body"));
            }
            // a field, whose initializer runs as a method on the instance or the class
            let function = if self.eat_punct("=")? {
                let saved_field = self.context;
                self.context.in_class_field = true;
                self.context.in_function = true;
                self.context.allow_super_member = true;
                self.context.allow_super_call = false;
                self.context.in_async = false;
                self.context.in_generator = false;
                let value_start = self.offset()?;
                let value = self.parse_assignment(true)?;
                self.context = saved_field;
                Some(Rc::new(FunctionNode {
                    name: Self::key_name(&key),
                    params: Vec::new(),
                    rest: None,
                    body: vec![Stmt::Return(Some(value))],
                    kind: FunctionKind::Method,
                    is_async: false,
                    is_generator: false,
                    strict: true,
                    source: (value_start, self.previous_end),
                }))
            } else {
                None
            };
            self.consume_semicolon()?;
            members.push(ClassMember {
                key,
                is_static,
                kind: ClassMemberKind::Field,
                function,
            });
        }
        self.context = saved;
        Ok(Rc::new(ClassNode {
            name,
            superclass,
            constructor,
            members,
            source: (start, self.previous_end),
        }))
    }

    fn parse_class_body_function(
        &mut self,
        start: usize,
        _is_static: bool,
    ) -> Result<Rc<FunctionNode>, ScriptError> {
        let saved = self.context;
        self.context.in_function = true;
        self.context.in_async = false;
        self.context.in_generator = false;
        self.context.allow_super_member = true;
        self.context.allow_super_call = false;
        self.context.in_class_field = true;
        self.expect_punct("{")?;
        let body = self.parse_block_body()?;
        self.context = saved;
        Ok(Rc::new(FunctionNode {
            name: Some("static".into()),
            params: Vec::new(),
            rest: None,
            body,
            kind: FunctionKind::Method,
            is_async: false,
            is_generator: false,
            strict: true,
            source: (start, self.previous_end),
        }))
    }

    // ---- modules ----

    fn parse_module_export_name(&mut self) -> Result<Name, ScriptError> {
        if let TokenKind::String(value) = self.peek_kind()? {
            self.advance()?;
            return Ok(value.into());
        }
        self.parse_identifier_name()
    }

    fn parse_module_specifier(&mut self) -> Result<Name, ScriptError> {
        match self.advance()?.kind {
            TokenKind::String(value) => {
                // import attributes such as with { type: "json" } are read and kept aside
                if (self.is_word("with")? || self.is_word("assert")?)
                    && !self.newline_before_current()?
                {
                    self.advance()?;
                    self.parse_object_literal()?;
                }
                Ok(value.into())
            }
            _ => Err(self.error_here("expected a module specifier string")),
        }
    }

    fn parse_import(&mut self) -> Result<ImportDeclaration, ScriptError> {
        self.advance()?;
        if let TokenKind::String(_) = self.peek_kind()? {
            let specifier = self.parse_module_specifier()?;
            self.consume_semicolon()?;
            return Ok(ImportDeclaration {
                specifier,
                bindings: Vec::new(),
            });
        }
        let mut bindings = Vec::new();
        if self.is_binding_identifier()? {
            let local = self.parse_binding_identifier()?;
            bindings.push(ImportBinding::Named {
                imported: "default".into(),
                local,
            });
            if !self.eat_punct(",")? {
                self.expect_word("from")?;
                let specifier = self.parse_module_specifier()?;
                self.consume_semicolon()?;
                return Ok(ImportDeclaration {
                    specifier,
                    bindings,
                });
            }
        }
        if self.eat_punct("*")? {
            self.expect_word("as")?;
            bindings.push(ImportBinding::Namespace(self.parse_binding_identifier()?));
        } else {
            self.expect_punct("{")?;
            while !self.eat_punct("}")? {
                let imported = self.parse_module_export_name()?;
                let local = if self.eat_word("as")? {
                    self.parse_binding_identifier()?
                } else {
                    imported.clone()
                };
                bindings.push(ImportBinding::Named { imported, local });
                if !self.is_punct("}")? {
                    self.expect_punct(",")?;
                }
            }
        }
        self.expect_word("from")?;
        let specifier = self.parse_module_specifier()?;
        self.consume_semicolon()?;
        Ok(ImportDeclaration {
            specifier,
            bindings,
        })
    }

    fn parse_export(&mut self) -> Result<Vec<ModuleItem>, ScriptError> {
        if self.eat_punct("*")? {
            let alias = if self.eat_word("as")? {
                Some(self.parse_module_export_name()?)
            } else {
                None
            };
            self.expect_word("from")?;
            let from = self.parse_module_specifier()?;
            self.consume_semicolon()?;
            return Ok(vec![ModuleItem::Export(ExportDeclaration::All {
                from,
                alias,
            })]);
        }
        if self.eat_word("default")? {
            let local: Name = "*default*".into();
            let start = self.offset()?;
            let statement = if self.is_word("function")? {
                self.advance()?;
                self.parse_default_function(start, false)?
            } else if self.is_word("async")? && self.is_word_at(1, "function")? {
                self.advance()?;
                self.advance()?;
                self.parse_default_function(start, true)?
            } else if self.is_word("class")? {
                let mut class = self.parse_class(false)?;
                if class.name.is_none() {
                    Rc::make_mut(&mut class).name = Some("default".into());
                }
                Stmt::Class(class)
            } else {
                let value = self.parse_assignment(true)?;
                self.consume_semicolon()?;
                Stmt::Declaration {
                    kind: DeclarationKind::Const,
                    declarators: vec![Declarator {
                        target: Pattern::Identifier(local.clone()),
                        init: Some(value),
                    }],
                }
            };
            let local = match &statement {
                Stmt::Function(function) => function.name.clone().unwrap_or(local),
                Stmt::Class(class) => class.name.clone().unwrap_or(local),
                _ => local,
            };
            return Ok(vec![ModuleItem::Export(ExportDeclaration::Default {
                local,
                statement,
            })]);
        }
        if self.eat_punct("{")? {
            let mut bindings = Vec::new();
            while !self.eat_punct("}")? {
                let local = self.parse_module_export_name()?;
                let exported = if self.eat_word("as")? {
                    self.parse_module_export_name()?
                } else {
                    local.clone()
                };
                bindings.push((local, exported));
                if !self.is_punct("}")? {
                    self.expect_punct(",")?;
                }
            }
            let from = if self.eat_word("from")? {
                Some(self.parse_module_specifier()?)
            } else {
                None
            };
            self.consume_semicolon()?;
            return Ok(vec![ModuleItem::Export(ExportDeclaration::Named {
                bindings,
                from,
            })]);
        }
        let statement = if self.is_word("var")? {
            self.parse_statement()?
        } else {
            self.parse_statement_list_item()?
        };
        if !matches!(
            statement,
            Stmt::Declaration { .. } | Stmt::Function(_) | Stmt::Class(_)
        ) {
            return Err(self.error_here("expected a declaration after export"));
        }
        Ok(vec![ModuleItem::Export(ExportDeclaration::Declaration(
            statement,
        ))])
    }

    fn parse_default_function(
        &mut self,
        start: usize,
        is_async: bool,
    ) -> Result<Stmt, ScriptError> {
        let is_generator = self.eat_punct("*")?;
        let name = if self.is_binding_identifier()? {
            self.parse_binding_identifier()?
        } else {
            "default".into()
        };
        Ok(Stmt::Function(self.parse_function_rest(
            start,
            Some(name),
            FunctionKind::Normal,
            is_async,
            is_generator,
            false,
        )?))
    }
}

pub(super) fn parse_script(source: &str) -> Result<Script, ScriptError> {
    let mut parser = Parser::new(source);
    parser.context.in_function = false;
    parser.parse_script()
}

pub(super) fn parse_module(source: &str) -> Result<Module, ScriptError> {
    Parser::new(source).parse_module()
}

// a function body for the Function constructor: parameters and body text are parsed together
pub(super) fn parse_function_source(
    parameters: &str,
    body: &str,
    is_async: bool,
    is_generator: bool,
) -> Result<Rc<FunctionNode>, ScriptError> {
    let source = format!(
        "({}function{} anonymous({parameters}\n) {{\n{body}\n}})",
        if is_async { "async " } else { "" },
        if is_generator { "*" } else { "" }
    );
    match Parser::new(&source).parse_standalone_expression()? {
        Expr::Function(function) => Ok(function),
        _ => Err(ScriptError {
            offset: 0,
            message: "invalid function source".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(source: &str) -> Vec<Stmt> {
        parse_script(source)
            .unwrap_or_else(|error| panic!("{source}: {} at {}", error.message, error.offset))
            .body
    }

    fn expression(source: &str) -> Expr {
        match script(source).remove(0) {
            Stmt::Expression(expression) => expression,
            other => panic!("expected an expression statement, got {other:?}"),
        }
    }

    fn fails(source: &str) {
        assert!(parse_script(source).is_err(), "{source} should not parse");
    }

    #[test]
    fn operators_follow_javascript_precedence_and_associativity() {
        let Expr::Binary {
            operator: BinaryOperator::Add,
            right,
            ..
        } = expression("a + b * c ** d ** e")
        else {
            panic!("addition is outermost");
        };
        let Expr::Binary {
            operator: BinaryOperator::Multiply,
            right,
            ..
        } = *right
        else {
            panic!("multiplication binds next");
        };
        assert!(matches!(
            *right,
            Expr::Binary {
                operator: BinaryOperator::Exponent,
                right: ref inner,
                ..
            } if matches!(**inner, Expr::Binary { operator: BinaryOperator::Exponent, .. })
        ));
        assert!(matches!(
            expression("a ?? b || c"),
            Expr::Logical {
                operator: LogicalOperator::Nullish,
                ..
            }
        ));
        assert!(matches!(
            expression("x = y += 2"),
            Expr::Assign {
                operator: AssignOperator::Assign,
                ..
            }
        ));
        assert!(matches!(expression("a, b, c"), Expr::Sequence(items) if items.len() == 3));
        fails("-a ** 2");
    }

    #[test]
    fn statements_cover_every_control_form() {
        let body = script(
            "var a = 1, b; let [c, ...d] = e; const {f, g: h = 2, ...i} = j;
             for (let k = 0; k < 3; k++) { if (k) continue; else break; }
             for (const key in object) {} for (x.y of list) {} outer: while (true) { break outer; }
             do x++; while (x < 9) switch (v) { case 1: f(); default: g(); }
             try { risky(); } catch { } finally { done(); } label: { }
             function* gen() { yield 1; yield* other(); }
             async function load() { await fetch(); for await (const chunk of stream) {} }",
        );
        assert_eq!(body.len(), 13);
        assert!(
            matches!(&body[0], Stmt::Declaration { kind: DeclarationKind::Var, declarators } if declarators.len() == 2)
        );
        assert!(matches!(&body[3], Stmt::For { .. }));
        assert!(matches!(&body[4], Stmt::ForIn { .. }));
        assert!(matches!(
            &body[5],
            Stmt::ForOf {
                left: ForHead::Target(Pattern::Expression(_)),
                ..
            }
        ));
        assert!(matches!(&body[8], Stmt::Switch { cases, .. } if cases.len() == 2));
        assert!(matches!(&body[11], Stmt::Function(function) if function.is_generator));
        let Stmt::Declaration { declarators, .. } = &body[2] else {
            panic!("const declaration");
        };
        let mut names = Vec::new();
        bound_names(&declarators[0].target, &mut names);
        assert_eq!(names, ["f", "h", "i"].map(Name::from).to_vec());
    }

    #[test]
    fn automatic_semicolons_respect_restricted_productions() {
        let body = script("let a = 1\nlet b = a\n++b\nfunction f() { return\n42 }");
        assert_eq!(body.len(), 4);
        assert!(matches!(
            &body[2],
            Stmt::Expression(Expr::Update { prefix: true, .. })
        ));
        let Stmt::Function(function) = &body[3] else {
            panic!("function declaration");
        };
        assert_eq!(function.body.len(), 2);
        assert!(matches!(function.body[0], Stmt::Return(None)));
        fails("a b");
        fails("throw\nerror");
    }

    #[test]
    fn arrows_regexes_templates_and_optional_chains_parse() {
        assert!(
            matches!(expression("(a, {b}, [c] = [], ...d) => a"), Expr::Function(f) if f.kind == FunctionKind::Arrow && f.params.len() == 3 && f.rest.is_some())
        );
        assert!(matches!(expression("async x => await x"), Expr::Function(f) if f.is_async));
        assert!(matches!(expression("(a + b)"), Expr::Binary { .. }));
        assert!(matches!(
            expression("x = /a[/]b/g.test(y) / 2"),
            Expr::Assign { .. }
        ));
        assert!(
            matches!(expression("`a${b}c${`d${e}`}`"), Expr::Template { quasis, expressions } if quasis.len() == 3 && expressions.len() == 2)
        );
        assert!(
            matches!(expression("tag`x${1}y`"), Expr::TaggedTemplate { raw, .. } if raw.len() == 2)
        );
        assert!(matches!(
            expression("a?.b.c?.[d]?.(e)"),
            Expr::OptionalChain(_)
        ));
        assert!(matches!(expression("new a.b.C(1).d"), Expr::Member { .. }));
        assert!(
            matches!(expression("new Foo"), Expr::New { arguments, .. } if arguments.is_empty())
        );
        assert!(
            matches!(expression("[a, b] = [b, a]"), Expr::Assign { target, .. } if matches!(*target, Pattern::Array { .. }))
        );
        assert!(
            matches!(expression("({a = 1, b: {c}} = obj)"), Expr::Assign { target, .. } if matches!(*target, Pattern::Object { .. }))
        );
        assert!(matches!(
            expression("obj.class + obj.default + obj.new"),
            Expr::Binary { .. }
        ));
    }

    #[test]
    fn object_literals_take_every_property_form() {
        let Expr::Object(properties) = expression(
            "({ a, b: 1, 'c': 2, 3: 4, [d]: 5, e() {}, get f() { return 1 }, set f(v) {}, async g() {}, *h() {}, async *i() {}, ...j, __proto__: k, get: 1, set() {}, async: 2 })",
        ) else {
            panic!("object literal");
        };
        assert_eq!(properties.len(), 16);
        assert!(
            matches!(&properties[3], ObjectProperty::Value(PropertyKey::Static(key), _) if &**key == "3")
        );
        assert!(matches!(
            &properties[6],
            ObjectProperty::Method {
                kind: MethodKind::Getter,
                ..
            }
        ));
        assert!(matches!(&properties[12], ObjectProperty::Prototype(_)));
        assert!(
            matches!(&properties[13], ObjectProperty::Value(PropertyKey::Static(key), _) if &**key == "get")
        );
    }

    #[test]
    fn classes_take_fields_accessors_private_names_and_static_blocks() {
        let body = script(
            "class A extends B.C { #x = 1; static count = 0; y; constructor(v) { super(v); this.#x = v; }
               get x() { return this.#x } set x(v) { this.#x = v } static create() { return new A(1) }
               #secret() { return super.method() } static { A.count = 1 } async *stream() {} has(o) { return #x in o } }",
        );
        let Stmt::Class(class) = &body[0] else {
            panic!("class");
        };
        assert!(class.superclass.is_some());
        assert_eq!(
            class.constructor.as_ref().map(|c| c.kind),
            Some(FunctionKind::DerivedConstructor)
        );
        assert_eq!(class.members.len(), 10);
        assert!(
            class
                .members
                .iter()
                .any(|member| member.kind == ClassMemberKind::StaticBlock)
        );
        assert!(class.members.iter().any(
            |member| matches!(&member.key, PropertyKey::Private(name) if &**name == "secret")
        ));
        fails("class A { constructor() { super() } }");
        fails("function f() { super.x }");
    }

    #[test]
    fn modules_parse_every_import_and_export_form() {
        let module = parse_module(
            "import a, { b as c, default as d } from './x.js'; import * as ns from './y.js'; import './z.js';
             import json from './data.json' with { type: 'json' };
             export const e = 1, f = 2; export function g() {} export class H {}
             export { e as default2, f }; export * from './w.js'; export * as all from './v.js';
             export { q as r } from './u.js'; export default async function () {}
             const top = await import('./late.js'); console.log(import.meta.url);",
        )
        .unwrap_or_else(|error| panic!("{} at {}", error.message, error.offset));
        assert_eq!(module.items.len(), 14);
        assert!(
            matches!(&module.items[0], ModuleItem::Import(import) if import.bindings.len() == 3)
        );
        assert!(
            matches!(&module.items[11], ModuleItem::Export(ExportDeclaration::Default { local, .. }) if &**local == "default")
        );
    }

    #[test]
    fn strict_mode_and_reserved_words_are_enforced() {
        fails("'use strict'; var let = 1;");
        fails("'use strict'; delete x;");
        assert!(parse_script("var let = 1; var yield = 2;").is_ok());
        fails("var class = 1;");
        fails("function* g() { var yield = 1; }");
        fails("async function f() { var await = 1; }");
        fails("return 1");
        assert!(parse_script("function f() { 'use strict'; return 1 }").is_ok());
    }

    #[test]
    fn function_constructor_sources_parse() {
        let function = parse_function_source("a, b = 2", "return a + b", false, false)
            .expect("function source");
        assert_eq!(function.params.len(), 2);
        assert_eq!(function.length(), 1);
    }
}
