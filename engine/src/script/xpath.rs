// XPath 1.0 behind document.evaluate, XPathEvaluator and XPathExpression: an expression is
// tokenized and parsed by the XPath 1.0 grammar, then evaluated over the document tree with
// the HTML standard's lowercase name tests for HTML elements; numbers print the way Blink's
// XPath does, with six significant digits

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::operations::is_callable;
use super::realm::{Builder, Native};
use super::*;
use crate::dom::{Document, Namespace, NodeKind};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum XPathMember {
    ResultType,
    NumberValue,
    StringValue,
    BooleanValue,
    SingleNodeValue,
    InvalidIteratorState,
    SnapshotLength,
    IterateNext,
    SnapshotItem,
    ExpressionEvaluate,
    Evaluate,
    CreateExpression,
    CreateNsResolver,
    Constructor,
}

pub(super) enum XPathObject {
    Result(RefCell<XPathResult>),
    Expression(Rc<Compiled>),
    Evaluator,
}

pub(super) struct XPathResult {
    kind: u16,
    value: ResultValue,
    cursor: usize,
    // the document's version when an iterator was made; a later change ends the iteration
    version: u64,
}

enum ResultValue {
    Number(f64),
    String(String),
    Boolean(bool),
    Nodes(Vec<Value>),
}

pub(super) struct XPathInterfaces {
    result: ObjectHandle,
    expression: ObjectHandle,
}

const RESULT_TYPES: [(&str, u16); 10] = [
    ("ANY_TYPE", 0),
    ("NUMBER_TYPE", 1),
    ("STRING_TYPE", 2),
    ("BOOLEAN_TYPE", 3),
    ("UNORDERED_NODE_ITERATOR_TYPE", 4),
    ("ORDERED_NODE_ITERATOR_TYPE", 5),
    ("UNORDERED_NODE_SNAPSHOT_TYPE", 6),
    ("ORDERED_NODE_SNAPSHOT_TYPE", 7),
    ("ANY_UNORDERED_NODE_TYPE", 8),
    ("FIRST_ORDERED_NODE_TYPE", 9),
];

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> XPathInterfaces {
    let plain = || {
        new_object(JsObject::with_prototype(
            Some(object_prototype.clone()),
            ObjectClass::Ordinary,
        ))
    };
    let method = |target: &ObjectHandle, name: &str, member: XPathMember, length: u32| {
        let function = build.function(Native::XPath(member), name, length);
        target.borrow_mut().define(
            name.to_string(),
            Property {
                enumerable: true,
                ..Property::data(Value::Object(function))
            },
        );
    };
    let getter = |target: &ObjectHandle, name: &str, member: XPathMember| {
        let get = build.function(Native::XPath(member), &format!("get {name}"), 0);
        target.borrow_mut().define(
            name.to_string(),
            Property {
                slot: Slot::accessor(Some(Value::Object(get)), None),
                enumerable: true,
                writable: false,
                configurable: true,
            },
        );
    };
    let constant = |target: &ObjectHandle, name: &str, value: u16| {
        target.borrow_mut().define(
            name.to_string(),
            Property {
                enumerable: true,
                writable: false,
                configurable: false,
                ..Property::data(Value::Number(f64::from(value)))
            },
        );
    };
    let tag = |target: &ObjectHandle, name: &str| {
        target.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property {
                writable: false,
                ..Property::hidden(Value::String(name.into()))
            },
        );
    };

    let result = plain();
    for (name, member) in [
        ("resultType", XPathMember::ResultType),
        ("numberValue", XPathMember::NumberValue),
        ("stringValue", XPathMember::StringValue),
        ("booleanValue", XPathMember::BooleanValue),
        ("singleNodeValue", XPathMember::SingleNodeValue),
        ("invalidIteratorState", XPathMember::InvalidIteratorState),
        ("snapshotLength", XPathMember::SnapshotLength),
    ] {
        getter(&result, name, member);
    }
    method(&result, "iterateNext", XPathMember::IterateNext, 0);
    method(&result, "snapshotItem", XPathMember::SnapshotItem, 1);
    tag(&result, "XPathResult");
    let result_constructor =
        build.constructor(Native::IllegalConstructor, "XPathResult", 0, &result);
    for (name, value) in RESULT_TYPES {
        constant(&result, name, value);
        constant(&result_constructor, name, value);
    }
    globals.push(("XPathResult", Value::Object(result_constructor)));

    let expression = plain();
    method(&expression, "evaluate", XPathMember::ExpressionEvaluate, 1);
    tag(&expression, "XPathExpression");
    let made = build.constructor(
        Native::IllegalConstructor,
        "XPathExpression",
        0,
        &expression,
    );
    globals.push(("XPathExpression", Value::Object(made)));

    let evaluator = plain();
    method(
        &evaluator,
        "createExpression",
        XPathMember::CreateExpression,
        1,
    );
    method(
        &evaluator,
        "createNSResolver",
        XPathMember::CreateNsResolver,
        1,
    );
    method(&evaluator, "evaluate", XPathMember::Evaluate, 2);
    tag(&evaluator, "XPathEvaluator");
    let made = build.constructor(
        Native::XPath(XPathMember::Constructor),
        "XPathEvaluator",
        0,
        &evaluator,
    );
    globals.push(("XPathEvaluator", Value::Object(made)));

    XPathInterfaces { result, expression }
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    LParen,
    RParen,
    LBracket,
    RBracket,
    Dot,
    DotDot,
    At,
    Comma,
    ColonColon,
    Slash,
    SlashSlash,
    Pipe,
    Plus,
    Minus,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Multiply,
    And,
    Or,
    Mod,
    Div,
    // a name test of *
    Star,
    // a name test of prefix:*
    PrefixStar(String),
    Name(Option<String>, String),
    Literal(String),
    Number(f64),
    Variable,
}

fn is_name_start(character: char) -> bool {
    character.is_alphabetic()
        || character == '_'
        || (!character.is_ascii() && !character.is_whitespace())
}

fn is_name_part(character: char) -> bool {
    is_name_start(character) || character.is_ascii_digit() || matches!(character, '.' | '-')
}

fn tokenize(text: &str) -> Option<Vec<Token>> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens: Vec<Token> = Vec::new();
    let mut at = 0;
    let read_name = |at: &mut usize| -> String {
        let start = *at;
        while *at < chars.len() && is_name_part(chars[*at]) {
            *at += 1;
        }
        chars[start..*at].iter().collect()
    };
    while at < chars.len() {
        let character = chars[at];
        if matches!(character, ' ' | '\t' | '\r' | '\n') {
            at += 1;
            continue;
        }
        // after a token that can end an operand, * multiplies and a name is an operator
        let operand_ended = tokens.last().is_some_and(|last| {
            matches!(
                last,
                Token::RParen
                    | Token::RBracket
                    | Token::Dot
                    | Token::DotDot
                    | Token::Star
                    | Token::PrefixStar(_)
                    | Token::Name(..)
                    | Token::Literal(_)
                    | Token::Number(_)
                    | Token::Variable
            )
        });
        let next = chars.get(at + 1).copied();
        let token = match character {
            '(' => Token::LParen,
            ')' => Token::RParen,
            '[' => Token::LBracket,
            ']' => Token::RBracket,
            '@' => Token::At,
            ',' => Token::Comma,
            '|' => Token::Pipe,
            '+' => Token::Plus,
            '-' => Token::Minus,
            '=' => Token::Equal,
            '.' if next == Some('.') => {
                at += 1;
                Token::DotDot
            }
            '.' if !next.is_some_and(|next| next.is_ascii_digit()) => Token::Dot,
            ':' if next == Some(':') => {
                at += 1;
                Token::ColonColon
            }
            '/' if next == Some('/') => {
                at += 1;
                Token::SlashSlash
            }
            '/' => Token::Slash,
            '!' if next == Some('=') => {
                at += 1;
                Token::NotEqual
            }
            '<' | '>' => {
                let equal = next == Some('=');
                if equal {
                    at += 1;
                }
                match (character, equal) {
                    ('<', false) => Token::Less,
                    ('<', true) => Token::LessEqual,
                    (_, false) => Token::Greater,
                    _ => Token::GreaterEqual,
                }
            }
            '*' if operand_ended => Token::Multiply,
            '*' => Token::Star,
            '"' | '\'' => {
                let end = chars[at + 1..]
                    .iter()
                    .position(|&found| found == character)?;
                let literal = chars[at + 1..at + 1 + end].iter().collect();
                at += end + 2;
                tokens.push(Token::Literal(literal));
                continue;
            }
            '0'..='9' | '.' => {
                let start = at;
                while at < chars.len() && chars[at].is_ascii_digit() {
                    at += 1;
                }
                if at < chars.len() && chars[at] == '.' {
                    at += 1;
                    while at < chars.len() && chars[at].is_ascii_digit() {
                        at += 1;
                    }
                }
                let digits: String = chars[start..at].iter().collect();
                tokens.push(Token::Number(digits.parse().ok()?));
                continue;
            }
            '$' => {
                at += 1;
                if !chars.get(at).is_some_and(|&found| is_name_start(found)) {
                    return None;
                }
                read_name(&mut at);
                if chars.get(at) == Some(&':')
                    && chars.get(at + 1).is_some_and(|&found| is_name_start(found))
                {
                    at += 1;
                    read_name(&mut at);
                }
                tokens.push(Token::Variable);
                continue;
            }
            found if is_name_start(found) => {
                let name = read_name(&mut at);
                if operand_ended {
                    tokens.push(match name.as_str() {
                        "and" => Token::And,
                        "or" => Token::Or,
                        "mod" => Token::Mod,
                        "div" => Token::Div,
                        _ => return None,
                    });
                    continue;
                }
                if chars.get(at) == Some(&':') && chars.get(at + 1) != Some(&':') {
                    at += 1;
                    if chars.get(at) == Some(&'*') {
                        at += 1;
                        tokens.push(Token::PrefixStar(name));
                    } else if chars.get(at).is_some_and(|&found| is_name_start(found)) {
                        let local = read_name(&mut at);
                        tokens.push(Token::Name(Some(name), local));
                    } else {
                        return None;
                    }
                } else {
                    tokens.push(Token::Name(None, name));
                }
                continue;
            }
            _ => return None,
        };
        at += 1;
        tokens.push(token);
    }
    Some(tokens)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Axis {
    Ancestor,
    AncestorOrSelf,
    Attribute,
    Child,
    Descendant,
    DescendantOrSelf,
    Following,
    FollowingSibling,
    Namespace,
    Parent,
    Preceding,
    PrecedingSibling,
    SelfNode,
}

impl Axis {
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "ancestor" => Axis::Ancestor,
            "ancestor-or-self" => Axis::AncestorOrSelf,
            "attribute" => Axis::Attribute,
            "child" => Axis::Child,
            "descendant" => Axis::Descendant,
            "descendant-or-self" => Axis::DescendantOrSelf,
            "following" => Axis::Following,
            "following-sibling" => Axis::FollowingSibling,
            "namespace" => Axis::Namespace,
            "parent" => Axis::Parent,
            "preceding" => Axis::Preceding,
            "preceding-sibling" => Axis::PrecedingSibling,
            "self" => Axis::SelfNode,
            _ => return None,
        })
    }
}

enum Test {
    Node,
    Text,
    Comment,
    ProcessingInstruction,
    // * or prefix:*, with the namespace the prefix names
    AnyName(Option<String>),
    // a name, with the namespace its prefix names
    Name(Option<String>, String),
}

struct Step {
    axis: Axis,
    test: Test,
    predicates: Vec<Expr>,
}

#[derive(Clone, Copy)]
enum Compare {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

#[derive(Clone, Copy)]
enum Arithmetic {
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
}

// a parsed expression, kept by an XPathExpression
pub(super) struct Compiled(Expr);

enum Expr {
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Compare(Compare, Box<Expr>, Box<Expr>),
    Arithmetic(Arithmetic, Box<Expr>, Box<Expr>),
    Negate(Box<Expr>),
    Union(Box<Expr>, Box<Expr>),
    // a location path from the context node, the root, or the nodes an expression gives
    Path(Option<Box<Expr>>, bool, Vec<Step>),
    Filter(Box<Expr>, Vec<Expr>),
    Literal(String),
    Number(f64),
    // a variable, which no binding gives a value, so it reads as the empty string
    Variable,
    Call(&'static str, Vec<Expr>),
}

// the core function library with the arguments each takes
const FUNCTIONS: &[(&str, usize, usize)] = &[
    ("last", 0, 0),
    ("position", 0, 0),
    ("count", 1, 1),
    ("id", 1, 1),
    ("local-name", 0, 1),
    ("namespace-uri", 0, 1),
    ("name", 0, 1),
    ("string", 0, 1),
    ("concat", 2, usize::MAX),
    ("starts-with", 2, 2),
    ("contains", 2, 2),
    ("substring-before", 2, 2),
    ("substring-after", 2, 2),
    ("substring", 2, 3),
    ("string-length", 0, 1),
    ("normalize-space", 0, 1),
    ("translate", 3, 3),
    ("boolean", 1, 1),
    ("not", 1, 1),
    ("true", 0, 0),
    ("false", 0, 0),
    ("lang", 1, 1),
    ("number", 0, 1),
    ("sum", 1, 1),
    ("floor", 1, 1),
    ("ceiling", 1, 1),
    ("round", 1, 1),
];

const NODE_TYPES: [&str; 4] = ["comment", "text", "processing-instruction", "node"];

struct Parser<'a> {
    tokens: Vec<Token>,
    at: usize,
    namespaces: &'a HashMap<String, String>,
}

type Parsed<T> = Result<T, ()>;

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn peek_at(&self, offset: usize) -> Option<&Token> {
        self.tokens.get(self.at + offset)
    }

    fn eat(&mut self, token: &Token) -> bool {
        if self.peek() == Some(token) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, token: &Token) -> Parsed<()> {
        if self.eat(token) { Ok(()) } else { Err(()) }
    }

    fn or(&mut self) -> Parsed<Expr> {
        let mut left = self.and()?;
        while self.eat(&Token::Or) {
            left = Expr::Or(Box::new(left), Box::new(self.and()?));
        }
        Ok(left)
    }

    fn and(&mut self) -> Parsed<Expr> {
        let mut left = self.equality()?;
        while self.eat(&Token::And) {
            left = Expr::And(Box::new(left), Box::new(self.equality()?));
        }
        Ok(left)
    }

    fn equality(&mut self) -> Parsed<Expr> {
        let mut left = self.relational()?;
        loop {
            let compare = match self.peek() {
                Some(Token::Equal) => Compare::Equal,
                Some(Token::NotEqual) => Compare::NotEqual,
                _ => return Ok(left),
            };
            self.at += 1;
            left = Expr::Compare(compare, Box::new(left), Box::new(self.relational()?));
        }
    }

    fn relational(&mut self) -> Parsed<Expr> {
        let mut left = self.additive()?;
        loop {
            let compare = match self.peek() {
                Some(Token::Less) => Compare::Less,
                Some(Token::LessEqual) => Compare::LessEqual,
                Some(Token::Greater) => Compare::Greater,
                Some(Token::GreaterEqual) => Compare::GreaterEqual,
                _ => return Ok(left),
            };
            self.at += 1;
            left = Expr::Compare(compare, Box::new(left), Box::new(self.additive()?));
        }
    }

    fn additive(&mut self) -> Parsed<Expr> {
        let mut left = self.multiplicative()?;
        loop {
            let operation = match self.peek() {
                Some(Token::Plus) => Arithmetic::Add,
                Some(Token::Minus) => Arithmetic::Subtract,
                _ => return Ok(left),
            };
            self.at += 1;
            left = Expr::Arithmetic(operation, Box::new(left), Box::new(self.multiplicative()?));
        }
    }

    fn multiplicative(&mut self) -> Parsed<Expr> {
        let mut left = self.unary()?;
        loop {
            let operation = match self.peek() {
                Some(Token::Multiply) => Arithmetic::Multiply,
                Some(Token::Div) => Arithmetic::Divide,
                Some(Token::Mod) => Arithmetic::Modulo,
                _ => return Ok(left),
            };
            self.at += 1;
            left = Expr::Arithmetic(operation, Box::new(left), Box::new(self.unary()?));
        }
    }

    fn unary(&mut self) -> Parsed<Expr> {
        if self.eat(&Token::Minus) {
            return Ok(Expr::Negate(Box::new(self.unary()?)));
        }
        let mut left = self.path()?;
        while self.eat(&Token::Pipe) {
            left = Expr::Union(Box::new(left), Box::new(self.path()?));
        }
        Ok(left)
    }

    fn starts_filter(&self) -> bool {
        match self.peek() {
            Some(Token::Variable | Token::LParen | Token::Literal(_) | Token::Number(_)) => true,
            Some(Token::Name(None, name)) => {
                self.peek_at(1) == Some(&Token::LParen) && !NODE_TYPES.contains(&name.as_str())
            }
            Some(Token::Name(Some(_), _)) => self.peek_at(1) == Some(&Token::LParen),
            _ => false,
        }
    }

    fn starts_step(&self) -> bool {
        matches!(
            self.peek(),
            Some(
                Token::Dot
                    | Token::DotDot
                    | Token::At
                    | Token::Star
                    | Token::PrefixStar(_)
                    | Token::Name(..)
            )
        )
    }

    fn path(&mut self) -> Parsed<Expr> {
        if self.starts_filter() {
            let primary = self.primary()?;
            let mut predicates = Vec::new();
            while self.peek() == Some(&Token::LBracket) {
                predicates.push(self.predicate()?);
            }
            let filter = if predicates.is_empty() {
                primary
            } else {
                Expr::Filter(Box::new(primary), predicates)
            };
            let mut steps = Vec::new();
            match self.peek() {
                Some(Token::Slash) => {
                    self.at += 1;
                    self.relative(&mut steps)?;
                }
                Some(Token::SlashSlash) => {
                    self.at += 1;
                    steps.push(descendant_or_self());
                    self.relative(&mut steps)?;
                }
                _ => return Ok(filter),
            }
            return Ok(Expr::Path(Some(Box::new(filter)), false, steps));
        }
        let mut steps = Vec::new();
        match self.peek() {
            Some(Token::Slash) => {
                self.at += 1;
                if self.starts_step() {
                    self.relative(&mut steps)?;
                }
                Ok(Expr::Path(None, true, steps))
            }
            Some(Token::SlashSlash) => {
                self.at += 1;
                steps.push(descendant_or_self());
                self.relative(&mut steps)?;
                Ok(Expr::Path(None, true, steps))
            }
            _ => {
                self.relative(&mut steps)?;
                Ok(Expr::Path(None, false, steps))
            }
        }
    }

    fn relative(&mut self, steps: &mut Vec<Step>) -> Parsed<()> {
        steps.push(self.step()?);
        loop {
            match self.peek() {
                Some(Token::Slash) => self.at += 1,
                Some(Token::SlashSlash) => {
                    self.at += 1;
                    steps.push(descendant_or_self());
                }
                _ => return Ok(()),
            }
            steps.push(self.step()?);
        }
    }

    fn step(&mut self) -> Parsed<Step> {
        if self.eat(&Token::Dot) {
            return Ok(Step {
                axis: Axis::SelfNode,
                test: Test::Node,
                predicates: Vec::new(),
            });
        }
        if self.eat(&Token::DotDot) {
            return Ok(Step {
                axis: Axis::Parent,
                test: Test::Node,
                predicates: Vec::new(),
            });
        }
        let axis = if self.eat(&Token::At) {
            Axis::Attribute
        } else if let (Some(Token::Name(None, name)), Some(Token::ColonColon)) =
            (self.peek(), self.peek_at(1))
        {
            let axis = Axis::named(name).ok_or(())?;
            self.at += 2;
            axis
        } else {
            Axis::Child
        };
        let test = match self.peek().cloned() {
            Some(Token::Star) => {
                self.at += 1;
                Test::AnyName(None)
            }
            Some(Token::PrefixStar(prefix)) => {
                self.at += 1;
                Test::AnyName(Some(self.namespace(&prefix)?))
            }
            Some(Token::Name(None, name))
                if self.peek_at(1) == Some(&Token::LParen)
                    && NODE_TYPES.contains(&name.as_str()) =>
            {
                self.at += 2;
                let test = match name.as_str() {
                    "comment" => Test::Comment,
                    "text" => Test::Text,
                    "node" => Test::Node,
                    _ => {
                        if let Some(Token::Literal(_)) = self.peek() {
                            self.at += 1;
                        }
                        Test::ProcessingInstruction
                    }
                };
                self.expect(&Token::RParen)?;
                test
            }
            Some(Token::Name(prefix, local)) => {
                self.at += 1;
                let namespace = match prefix {
                    Some(prefix) => Some(self.namespace(&prefix)?),
                    None => None,
                };
                Test::Name(namespace, local)
            }
            _ => return Err(()),
        };
        let mut predicates = Vec::new();
        while self.peek() == Some(&Token::LBracket) {
            predicates.push(self.predicate()?);
        }
        Ok(Step {
            axis,
            test,
            predicates,
        })
    }

    fn namespace(&self, prefix: &str) -> Parsed<String> {
        self.namespaces.get(prefix).cloned().ok_or(())
    }

    fn predicate(&mut self) -> Parsed<Expr> {
        self.expect(&Token::LBracket)?;
        let expression = self.or()?;
        self.expect(&Token::RBracket)?;
        Ok(expression)
    }

    fn primary(&mut self) -> Parsed<Expr> {
        match self.peek().cloned() {
            Some(Token::Variable) => {
                self.at += 1;
                Ok(Expr::Variable)
            }
            Some(Token::LParen) => {
                self.at += 1;
                let inner = self.or()?;
                self.expect(&Token::RParen)?;
                Ok(inner)
            }
            Some(Token::Literal(text)) => {
                self.at += 1;
                Ok(Expr::Literal(text))
            }
            Some(Token::Number(number)) => {
                self.at += 1;
                Ok(Expr::Number(number))
            }
            Some(Token::Name(None, name)) => {
                let &(function, least, most) = FUNCTIONS
                    .iter()
                    .find(|(known, ..)| *known == name)
                    .ok_or(())?;
                self.at += 1;
                self.expect(&Token::LParen)?;
                let mut arguments = Vec::new();
                if !self.eat(&Token::RParen) {
                    loop {
                        arguments.push(self.or()?);
                        if self.eat(&Token::RParen) {
                            break;
                        }
                        self.expect(&Token::Comma)?;
                    }
                }
                if arguments.len() < least || arguments.len() > most {
                    return Err(());
                }
                Ok(Expr::Call(function, arguments))
            }
            _ => Err(()),
        }
    }
}

fn descendant_or_self() -> Step {
    Step {
        axis: Axis::DescendantOrSelf,
        test: Test::Node,
        predicates: Vec::new(),
    }
}

// an XPath node: a tree node, or the attribute at an index of an element
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum XNode {
    Node(NodeId),
    Attribute(NodeId, usize),
}

enum XValue {
    Nodes(Vec<XNode>),
    Boolean(bool),
    Number(f64),
    String(String),
}

// a node set was needed and something else came
struct NotNodes;

struct Evaluation<'a> {
    document: &'a Document,
    order: HashMap<NodeId, usize>,
    counted: usize,
}

#[derive(Clone, Copy)]
struct Context {
    node: XNode,
    position: usize,
    size: usize,
}

// XPath's number from a string: optional whitespace, an optional minus, digits with an
// optional fraction, and nothing else
fn string_number(text: &str) -> f64 {
    let trimmed = text.trim_matches([' ', '\t', '\r', '\n']);
    let digits = trimmed.strip_prefix('-').unwrap_or(trimmed);
    let valid = !digits.is_empty()
        && digits
            .chars()
            .all(|found| found.is_ascii_digit() || found == '.')
        && digits.matches('.').count() <= 1
        && digits != ".";
    if valid {
        trimmed.parse().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    }
}

// a number as Blink's XPath prints it: six significant digits as %g would give them, in
// exponent form past a million or below a millionth
fn number_string(number: f64) -> String {
    if number.is_nan() {
        return "NaN".into();
    }
    if number == 0.0 {
        return "0".into();
    }
    if number.is_infinite() {
        return if number < 0.0 {
            "-Infinity"
        } else {
            "Infinity"
        }
        .into();
    }
    let sign = if number < 0.0 { "-" } else { "" };
    let formatted = format!("{:.5e}", number.abs());
    let (mantissa, exponent) = formatted.split_once('e').expect("exponent form");
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().expect("an exponent");
    let point = exponent + 1;
    if !(-5..=6).contains(&point) {
        let sign_of_exponent = if exponent < 0 { '-' } else { '+' };
        return format!(
            "{sign}{}.{}e{sign_of_exponent}{}",
            &digits[..1],
            &digits[1..],
            exponent.abs()
        );
    }
    let mut text = if point <= 0 {
        format!("0.{}{digits}", "0".repeat((-point) as usize))
    } else {
        let point = point as usize;
        format!("{}.{}", &digits[..point], &digits[point..])
    };
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    format!("{sign}{text}")
}

fn is_xml_space(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\r' | '\n')
}

impl<'a> Evaluation<'a> {
    fn new(document: &'a Document) -> Self {
        Evaluation {
            document,
            order: HashMap::new(),
            counted: 0,
        }
    }

    // a node's place in document order; each tree is numbered whole the first time one of
    // its nodes is asked about, and an element's attributes come right after it
    fn position(&mut self, node: XNode) -> usize {
        let (element, offset) = match node {
            XNode::Node(node) => (node, 0),
            XNode::Attribute(element, index) => (element, index + 1),
        };
        if let Some(&found) = self.order.get(&element) {
            return found + offset;
        }
        let mut root = element;
        while let Some(parent) = self.document.parent(root) {
            root = parent;
        }
        let mut stack = vec![root];
        while let Some(at) = stack.pop() {
            self.order.insert(at, self.counted);
            let attributes = self
                .document
                .element(at)
                .map_or(0, |element| element.attrs.len());
            self.counted += 1 + attributes;
            stack.extend(self.document.children(at).iter().rev());
        }
        self.order.get(&element).copied().unwrap_or(0) + offset
    }

    fn sort(&mut self, mut nodes: Vec<XNode>) -> Vec<XNode> {
        nodes.sort_by_cached_key(|&node| self.position(node));
        nodes.dedup();
        nodes
    }

    fn string_value(&self, node: XNode) -> String {
        match node {
            XNode::Attribute(element, index) => self
                .document
                .element(element)
                .and_then(|element| element.attrs.iter().nth(index))
                .map(|(_, value)| value.clone())
                .unwrap_or_default(),
            XNode::Node(node) => match self.document.kind(node) {
                Some(NodeKind::Text(text) | NodeKind::Comment(text)) => text.clone(),
                _ => {
                    let mut text = String::new();
                    let mut stack: Vec<NodeId> =
                        self.document.children(node).iter().rev().copied().collect();
                    while let Some(at) = stack.pop() {
                        match self.document.kind(at) {
                            Some(NodeKind::Text(data)) => text.push_str(data),
                            Some(NodeKind::Element(_)) => {
                                stack.extend(self.document.children(at).iter().rev())
                            }
                            _ => {}
                        }
                    }
                    text
                }
            },
        }
    }

    fn to_string(&self, value: &XValue) -> String {
        match value {
            XValue::Nodes(nodes) => nodes
                .first()
                .map(|&node| self.string_value(node))
                .unwrap_or_default(),
            XValue::Boolean(value) => value.to_string(),
            XValue::Number(number) => number_string(*number),
            XValue::String(text) => text.clone(),
        }
    }

    fn to_number(&self, value: &XValue) -> f64 {
        match value {
            XValue::Number(number) => *number,
            XValue::Boolean(value) => f64::from(u8::from(*value)),
            other => string_number(&self.to_string(other)),
        }
    }

    fn to_boolean(value: &XValue) -> bool {
        match value {
            XValue::Nodes(nodes) => !nodes.is_empty(),
            XValue::Boolean(value) => *value,
            XValue::Number(number) => *number != 0.0 && !number.is_nan(),
            XValue::String(text) => !text.is_empty(),
        }
    }

    fn descendants(&self, node: NodeId, into: &mut Vec<XNode>) {
        for &child in self.document.children(node) {
            into.push(XNode::Node(child));
            self.descendants(child, into);
        }
    }

    // the subtree in reverse document order, the deepest last node first
    fn reverse_subtree(&self, node: NodeId, into: &mut Vec<XNode>) {
        for &child in self.document.children(node).iter().rev() {
            self.reverse_subtree(child, into);
        }
        into.push(XNode::Node(node));
    }

    fn parent_of(&self, node: XNode) -> Option<NodeId> {
        match node {
            XNode::Node(node) => self.document.parent(node),
            XNode::Attribute(element, _) => Some(element),
        }
    }

    // the nodes along an axis in its own order: reverse axes go nearest first
    fn axis(&self, axis: Axis, node: XNode) -> Vec<XNode> {
        let mut nodes = Vec::new();
        let tree_node = match node {
            XNode::Node(node) => Some(node),
            XNode::Attribute(..) => None,
        };
        match axis {
            Axis::SelfNode => nodes.push(node),
            Axis::Child => {
                if let Some(node) = tree_node {
                    nodes.extend(
                        self.document
                            .children(node)
                            .iter()
                            .map(|&child| XNode::Node(child)),
                    );
                }
            }
            Axis::Descendant | Axis::DescendantOrSelf => {
                if axis == Axis::DescendantOrSelf {
                    nodes.push(node);
                }
                if let Some(node) = tree_node {
                    self.descendants(node, &mut nodes);
                }
            }
            Axis::Parent => nodes.extend(self.parent_of(node).map(XNode::Node)),
            Axis::Ancestor | Axis::AncestorOrSelf => {
                if axis == Axis::AncestorOrSelf {
                    nodes.push(node);
                }
                let mut at = self.parent_of(node);
                while let Some(parent) = at {
                    nodes.push(XNode::Node(parent));
                    at = self.document.parent(parent);
                }
            }
            Axis::FollowingSibling | Axis::PrecedingSibling => {
                if let Some(node) = tree_node {
                    let mut at = self.document.sibling(node, axis == Axis::FollowingSibling);
                    while let Some(sibling) = at {
                        nodes.push(XNode::Node(sibling));
                        at = self
                            .document
                            .sibling(sibling, axis == Axis::FollowingSibling);
                    }
                }
            }
            Axis::Following => {
                let mut at = match node {
                    XNode::Node(node) => node,
                    XNode::Attribute(element, _) => {
                        self.descendants(element, &mut nodes);
                        element
                    }
                };
                loop {
                    let mut sibling = self.document.sibling(at, true);
                    while let Some(next) = sibling {
                        nodes.push(XNode::Node(next));
                        self.descendants(next, &mut nodes);
                        sibling = self.document.sibling(next, true);
                    }
                    match self.document.parent(at) {
                        Some(parent) => at = parent,
                        None => break,
                    }
                }
            }
            Axis::Preceding => {
                let mut at = match node {
                    XNode::Node(node) => node,
                    XNode::Attribute(element, _) => element,
                };
                loop {
                    let mut sibling = self.document.sibling(at, false);
                    while let Some(previous) = sibling {
                        self.reverse_subtree(previous, &mut nodes);
                        sibling = self.document.sibling(previous, false);
                    }
                    match self.document.parent(at) {
                        Some(parent) => at = parent,
                        None => break,
                    }
                }
            }
            Axis::Attribute => {
                if let Some(element) = tree_node.and_then(|node| self.document.element(node)) {
                    let node = tree_node.expect("an element");
                    nodes.extend(
                        (0..element.attrs.len()).map(|index| XNode::Attribute(node, index)),
                    );
                }
            }
            Axis::Namespace => {}
        }
        nodes
    }

    fn matches(&self, test: &Test, axis: Axis, node: XNode) -> bool {
        match (test, node) {
            (Test::Node, _) => true,
            (Test::AnyName(None), XNode::Attribute(..)) => axis == Axis::Attribute,
            (Test::Name(None, name), XNode::Attribute(element, index)) => {
                axis == Axis::Attribute
                    && self
                        .document
                        .element(element)
                        .and_then(|element| element.attrs.iter().nth(index))
                        .is_some_and(|(attribute, _)| attribute == name)
            }
            (_, XNode::Attribute(..)) => false,
            (test, XNode::Node(node)) => {
                let kind = self.document.kind(node);
                match test {
                    Test::Text => matches!(kind, Some(NodeKind::Text(_))),
                    Test::Comment => matches!(kind, Some(NodeKind::Comment(_))),
                    Test::ProcessingInstruction => false,
                    Test::Node => true,
                    Test::AnyName(namespace) => {
                        axis != Axis::Attribute
                            && matches!(kind, Some(NodeKind::Element(element))
                                if namespace.as_ref().is_none_or(|uri| element.namespace.uri() == uri))
                    }
                    Test::Name(namespace, name) => {
                        axis != Axis::Attribute
                            && matches!(kind, Some(NodeKind::Element(element)) if match namespace {
                                // HTML elements match names without a prefix in any case;
                                // other elements have a namespace such a name cannot match
                                None => element.namespace == Namespace::Html
                                    && element.name == name.to_ascii_lowercase(),
                                Some(uri) => element.namespace.uri() == uri && element.name == *name,
                            })
                    }
                }
            }
        }
    }

    fn nodes(&mut self, expression: &Expr, context: Context) -> Result<Vec<XNode>, NotNodes> {
        match self.evaluate(expression, context)? {
            XValue::Nodes(nodes) => Ok(nodes),
            _ => Err(NotNodes),
        }
    }

    fn filter(&mut self, nodes: Vec<XNode>, predicates: &[Expr]) -> Result<Vec<XNode>, NotNodes> {
        let mut nodes = nodes;
        for predicate in predicates {
            let size = nodes.len();
            let mut kept = Vec::new();
            for (index, &node) in nodes.iter().enumerate() {
                let context = Context {
                    node,
                    position: index + 1,
                    size,
                };
                let keep = match self.evaluate(predicate, context)? {
                    XValue::Number(number) => number == (index + 1) as f64,
                    other => Self::to_boolean(&other),
                };
                if keep {
                    kept.push(node);
                }
            }
            nodes = kept;
        }
        Ok(nodes)
    }

    fn root_of(&self, node: XNode) -> NodeId {
        let mut at = match node {
            XNode::Node(node) => node,
            XNode::Attribute(element, _) => element,
        };
        while let Some(parent) = self.document.parent(at) {
            at = parent;
        }
        at
    }

    fn evaluate(&mut self, expression: &Expr, context: Context) -> Result<XValue, NotNodes> {
        Ok(match expression {
            Expr::Or(left, right) => XValue::Boolean(
                Self::to_boolean(&self.evaluate(left, context)?)
                    || Self::to_boolean(&self.evaluate(right, context)?),
            ),
            Expr::And(left, right) => XValue::Boolean(
                Self::to_boolean(&self.evaluate(left, context)?)
                    && Self::to_boolean(&self.evaluate(right, context)?),
            ),
            Expr::Compare(compare, left, right) => {
                let left = self.evaluate(left, context)?;
                let right = self.evaluate(right, context)?;
                XValue::Boolean(self.compare(*compare, &left, &right))
            }
            Expr::Arithmetic(operation, left, right) => {
                let left = self.evaluate(left, context)?;
                let left = self.to_number(&left);
                let right = self.evaluate(right, context)?;
                let right = self.to_number(&right);
                XValue::Number(match operation {
                    Arithmetic::Add => left + right,
                    Arithmetic::Subtract => left - right,
                    Arithmetic::Multiply => left * right,
                    Arithmetic::Divide => left / right,
                    Arithmetic::Modulo => left % right,
                })
            }
            Expr::Negate(inner) => {
                let value = self.evaluate(inner, context)?;
                XValue::Number(-self.to_number(&value))
            }
            Expr::Union(left, right) => {
                let mut nodes = self.nodes(left, context)?;
                nodes.extend(self.nodes(right, context)?);
                XValue::Nodes(self.sort(nodes))
            }
            Expr::Path(start, absolute, steps) => {
                let mut nodes = match start {
                    Some(start) => self.nodes(start, context)?,
                    None if *absolute => vec![XNode::Node(self.root_of(context.node))],
                    None => vec![context.node],
                };
                for step in steps {
                    let mut gathered = Vec::new();
                    for &node in &nodes {
                        let along: Vec<XNode> = self
                            .axis(step.axis, node)
                            .into_iter()
                            .filter(|&found| self.matches(&step.test, step.axis, found))
                            .collect();
                        gathered.extend(self.filter(along, &step.predicates)?);
                    }
                    nodes = self.sort(gathered);
                }
                XValue::Nodes(nodes)
            }
            Expr::Filter(inner, predicates) => {
                let nodes = self.nodes(inner, context)?;
                XValue::Nodes(self.filter(nodes, predicates)?)
            }
            Expr::Literal(text) => XValue::String(text.clone()),
            Expr::Number(number) => XValue::Number(*number),
            Expr::Variable => XValue::String(String::new()),
            Expr::Call(function, arguments) => self.call(function, arguments, context)?,
        })
    }

    fn compare(&self, compare: Compare, left: &XValue, right: &XValue) -> bool {
        let numbers = |left: f64, right: f64| match compare {
            Compare::Equal => left == right,
            Compare::NotEqual => left != right,
            Compare::Less => left < right,
            Compare::LessEqual => left <= right,
            Compare::Greater => left > right,
            Compare::GreaterEqual => left >= right,
        };
        let equality = matches!(compare, Compare::Equal | Compare::NotEqual);
        let strings = |left: &str, right: &str| {
            if equality {
                (left == right) == matches!(compare, Compare::Equal)
            } else {
                numbers(string_number(left), string_number(right))
            }
        };
        match (left, right) {
            (XValue::Nodes(left), XValue::Nodes(right)) => {
                let right: Vec<String> =
                    right.iter().map(|&node| self.string_value(node)).collect();
                left.iter().any(|&node| {
                    let value = self.string_value(node);
                    right.iter().any(|other| strings(&value, other))
                })
            }
            (XValue::Nodes(nodes), other) | (other, XValue::Nodes(nodes)) => {
                let flipped =
                    matches!(right, XValue::Nodes(_)) && !matches!(left, XValue::Nodes(_));
                let ordered = |node_side: f64, other: f64| {
                    if flipped {
                        numbers(other, node_side)
                    } else {
                        numbers(node_side, other)
                    }
                };
                match other {
                    XValue::Number(number) => nodes
                        .iter()
                        .any(|&node| ordered(string_number(&self.string_value(node)), *number)),
                    XValue::String(text) if equality => nodes
                        .iter()
                        .any(|&node| strings(&self.string_value(node), text)),
                    XValue::String(text) => {
                        let number = string_number(text);
                        nodes
                            .iter()
                            .any(|&node| ordered(string_number(&self.string_value(node)), number))
                    }
                    XValue::Boolean(value) => {
                        let set = f64::from(u8::from(!nodes.is_empty()));
                        ordered(set, f64::from(u8::from(*value)))
                    }
                    XValue::Nodes(_) => unreachable!("handled above"),
                }
            }
            (left, right) if equality => {
                if matches!(left, XValue::Boolean(_)) || matches!(right, XValue::Boolean(_)) {
                    (Self::to_boolean(left) == Self::to_boolean(right))
                        == matches!(compare, Compare::Equal)
                } else if matches!(left, XValue::Number(_)) || matches!(right, XValue::Number(_)) {
                    numbers(self.to_number(left), self.to_number(right))
                } else {
                    strings(&self.to_string(left), &self.to_string(right))
                }
            }
            (left, right) => numbers(self.to_number(left), self.to_number(right)),
        }
    }

    fn node_or_context(
        &mut self,
        arguments: &[Expr],
        context: Context,
    ) -> Result<Option<XNode>, NotNodes> {
        match arguments.first() {
            Some(argument) => Ok(self.nodes(argument, context)?.first().copied()),
            None => Ok(Some(context.node)),
        }
    }

    fn call(
        &mut self,
        function: &str,
        arguments: &[Expr],
        context: Context,
    ) -> Result<XValue, NotNodes> {
        let mut values = Vec::new();
        let takes_nodes = matches!(
            function,
            "count" | "sum" | "local-name" | "namespace-uri" | "name"
        );
        if !takes_nodes {
            for argument in arguments {
                values.push(self.evaluate(argument, context)?);
            }
        }
        let text = |evaluation: &Self, index: usize| -> String {
            match values.get(index) {
                Some(value) => evaluation.to_string(value),
                None => evaluation.string_value(context.node),
            }
        };
        let units = |text: &str| -> Vec<u16> { text.encode_utf16().collect() };
        Ok(match function {
            "last" => XValue::Number(context.size as f64),
            "position" => XValue::Number(context.position as f64),
            "count" => XValue::Number(self.nodes(&arguments[0], context)?.len() as f64),
            "sum" => {
                let nodes = self.nodes(&arguments[0], context)?;
                XValue::Number(
                    nodes
                        .iter()
                        .map(|&node| string_number(&self.string_value(node)))
                        .sum(),
                )
            }
            "local-name" | "name" | "namespace-uri" => {
                let node = self.node_or_context(arguments, context)?;
                XValue::String(
                    node.map(|node| self.node_name(node, function))
                        .unwrap_or_default(),
                )
            }
            "id" => {
                let tokens: Vec<String> = match &values[0] {
                    XValue::Nodes(nodes) => {
                        nodes.iter().map(|&node| self.string_value(node)).collect()
                    }
                    other => vec![self.to_string(other)],
                };
                let wanted: Vec<&str> = tokens
                    .iter()
                    .flat_map(|token| token.split(is_xml_space))
                    .filter(|token| !token.is_empty())
                    .collect();
                let mut found = Vec::new();
                let mut stack = vec![self.root_of(context.node)];
                let mut seen = std::collections::HashSet::new();
                while let Some(at) = stack.pop() {
                    if let Some(id) = self
                        .document
                        .element(at)
                        .and_then(|element| element.attrs.get("id"))
                        && wanted.contains(&id.as_str())
                        && seen.insert(id.clone())
                    {
                        found.push(XNode::Node(at));
                    }
                    stack.extend(self.document.children(at).iter().rev());
                }
                XValue::Nodes(self.sort(found))
            }
            "string" => XValue::String(text(self, 0)),
            "concat" => XValue::String(values.iter().map(|value| self.to_string(value)).collect()),
            "starts-with" => XValue::Boolean(text(self, 0).starts_with(&text(self, 1))),
            "contains" => XValue::Boolean(text(self, 0).contains(&text(self, 1))),
            "substring-before" => {
                let whole = text(self, 0);
                let part = text(self, 1);
                XValue::String(
                    whole
                        .find(&part)
                        .map(|at| whole[..at].to_string())
                        .unwrap_or_default(),
                )
            }
            "substring-after" => {
                let whole = text(self, 0);
                let part = text(self, 1);
                XValue::String(
                    whole
                        .find(&part)
                        .map(|at| whole[at + part.len()..].to_string())
                        .unwrap_or_default(),
                )
            }
            "substring" => {
                let whole = units(&text(self, 0));
                let start = round(self.to_number(&values[1]));
                let end = match values.get(2) {
                    Some(length) => start + round(self.to_number(length)),
                    None => f64::INFINITY,
                };
                let kept: Vec<u16> = whole
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| {
                        let position = (*index + 1) as f64;
                        position >= start && position < end
                    })
                    .map(|(_, &unit)| unit)
                    .collect();
                XValue::String(String::from_utf16_lossy(&kept))
            }
            "string-length" => XValue::Number(units(&text(self, 0)).len() as f64),
            "normalize-space" => XValue::String(
                text(self, 0)
                    .split(is_xml_space)
                    .filter(|word| !word.is_empty())
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            "translate" => {
                let from = units(&text(self, 1));
                let to = units(&text(self, 2));
                let mapped: Vec<u16> = units(&text(self, 0))
                    .into_iter()
                    .filter_map(|unit| match from.iter().position(|&found| found == unit) {
                        Some(index) => to.get(index).copied(),
                        None => Some(unit),
                    })
                    .collect();
                XValue::String(String::from_utf16_lossy(&mapped))
            }
            "boolean" => XValue::Boolean(Self::to_boolean(&values[0])),
            "not" => XValue::Boolean(!Self::to_boolean(&values[0])),
            "true" => XValue::Boolean(true),
            "false" => XValue::Boolean(false),
            "lang" => {
                let wanted = text(self, 0).to_ascii_lowercase();
                let mut at = match context.node {
                    XNode::Node(node) => Some(node),
                    XNode::Attribute(element, _) => Some(element),
                };
                let mut language = None;
                while let Some(node) = at {
                    if let Some(found) = self
                        .document
                        .element(node)
                        .and_then(|element| element.attrs.get("xml:lang"))
                    {
                        language = Some(found.to_ascii_lowercase());
                        break;
                    }
                    at = self.document.parent(node);
                }
                XValue::Boolean(language.is_some_and(|language| {
                    language == wanted || language.starts_with(&format!("{wanted}-"))
                }))
            }
            "number" => XValue::Number(match values.first() {
                Some(value) => self.to_number(value),
                None => string_number(&self.string_value(context.node)),
            }),
            "floor" => XValue::Number(self.to_number(&values[0]).floor()),
            "ceiling" => XValue::Number(self.to_number(&values[0]).ceil()),
            "round" => XValue::Number(round(self.to_number(&values[0]))),
            _ => unreachable!("the parser knows only the core functions"),
        })
    }

    fn node_name(&self, node: XNode, function: &str) -> String {
        match node {
            XNode::Attribute(element, index) => match function {
                "namespace-uri" => String::new(),
                _ => self
                    .document
                    .element(element)
                    .and_then(|element| element.attrs.iter().nth(index))
                    .map(|(name, _)| name.clone())
                    .unwrap_or_default(),
            },
            XNode::Node(node) => match self.document.element(node) {
                Some(element) if function == "namespace-uri" => element.namespace.uri().to_string(),
                Some(element) => element.name.clone(),
                None => String::new(),
            },
        }
    }
}

// XPath's round: halves go up, and what rounds to zero from below stays negative zero
fn round(number: f64) -> f64 {
    if number.is_nan() || number.is_infinite() {
        number
    } else if (-0.5..0.0).contains(&number) {
        -0.0
    } else {
        (number + 0.5).floor()
    }
}

impl ScriptRuntime {
    fn xpath_object(value: &Value) -> Option<Rc<XPathObject>> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::XPath(found) => Some(found.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    // compiles an expression, asking the resolver for the namespace of each prefix in it
    fn compile_xpath(
        &mut self,
        text: &str,
        resolver: &Value,
        failed: &str,
    ) -> Result<Rc<Compiled>, ScriptError> {
        let syntax = |runtime: &mut Self| {
            runtime.throw_value(dom_exception(
                "SyntaxError",
                &format!("{failed}: The string '{text}' is not a valid XPath expression."),
            ))
        };
        let Some(tokens) = tokenize(text) else {
            return Err(syntax(self));
        };
        let mut namespaces = HashMap::new();
        for token in &tokens {
            let prefix = match token {
                Token::Name(Some(prefix), _) | Token::PrefixStar(prefix) => prefix,
                _ => continue,
            };
            if namespaces.contains_key(prefix) {
                continue;
            }
            let found = match resolver {
                Value::Undefined | Value::Null => Value::Null,
                resolver if is_callable(resolver) => self.call_function(
                    resolver.clone(),
                    Value::Undefined,
                    vec![Value::String(prefix.as_str().into())],
                )?,
                resolver => {
                    let lookup = self.get_property(resolver, "lookupNamespaceURI")?;
                    if !is_callable(&lookup) {
                        return Err(self.type_error(format!(
                            "{failed}: The provided resolver does not have a lookupNamespaceURI method."
                        )));
                    }
                    self.call_function(
                        lookup,
                        resolver.clone(),
                        vec![Value::String(prefix.as_str().into())],
                    )?
                }
            };
            let uri = match found {
                Value::Undefined | Value::Null => None,
                value => Some(self.coerce_string(value)?).filter(|uri| !uri.is_empty()),
            };
            let Some(uri) = uri else {
                return Err(self.throw_value(dom_exception(
                    "NamespaceError",
                    &format!("{failed}: The string '{text}' contains unresolvable namespaces."),
                )));
            };
            namespaces.insert(prefix.clone(), uri);
        }
        let mut parser = Parser {
            tokens,
            at: 0,
            namespaces: &namespaces,
        };
        match parser.or() {
            Ok(expression) if parser.at == parser.tokens.len() => Ok(Rc::new(Compiled(expression))),
            _ => Err(syntax(self)),
        }
    }

    // an attribute as the attributes list hands it out
    fn attribute_value(&self, element: NodeId, index: usize) -> Value {
        let attribute = self.document.borrow().element(element).and_then(|found| {
            found
                .attrs
                .iter()
                .nth(index)
                .map(|(name, value)| (name.clone(), value.clone()))
        });
        let Some((name, value)) = attribute else {
            return Value::Null;
        };
        self.object_from_entries(vec![
            ("name".to_string(), Value::String(name.clone().into())),
            ("localName".to_string(), Value::String(name.clone().into())),
            ("nodeName".to_string(), Value::String(name.into())),
            ("value".to_string(), Value::String(value.clone().into())),
            ("nodeValue".to_string(), Value::String(value.into())),
            ("nodeType".to_string(), Value::Number(2.0)),
            ("namespaceURI".to_string(), Value::Null),
            ("specified".to_string(), Value::Bool(true)),
            ("ownerElement".to_string(), self.node_value(element)),
        ])
    }

    fn run_xpath(
        &mut self,
        expression: &Compiled,
        context: &Value,
        kind: Value,
        failed: &str,
    ) -> Result<Value, ScriptError> {
        let context = match context.resolve() {
            Value::Element(handle) => handle.id,
            Value::Document => self.document.borrow().root(),
            _ => {
                return Err(
                    self.type_error(format!("{failed}: parameter 2 is not of type 'Node'."))
                );
            }
        };
        let kind = match kind {
            Value::Undefined => 0,
            value => (self.coerce_number(value)?.trunc().rem_euclid(65536.0)) as u16,
        };
        let evaluated = {
            let document = self.document.borrow();
            let mut evaluation = Evaluation::new(&document);
            let start = Context {
                node: XNode::Node(context),
                position: 1,
                size: 1,
            };
            evaluation
                .evaluate(&expression.0, start)
                .map(|value| match value {
                    XValue::Nodes(nodes) => Err(nodes),
                    other => Ok(match (kind, other) {
                        (1, other) => ResultValue::Number(evaluation.to_number(&other)),
                        (2, other) => ResultValue::String(evaluation.to_string(&other)),
                        (3, other) => ResultValue::Boolean(Evaluation::to_boolean(&other)),
                        (_, XValue::Number(number)) => ResultValue::Number(number),
                        (_, XValue::String(text)) => ResultValue::String(text),
                        (_, XValue::Boolean(value)) => ResultValue::Boolean(value),
                        (_, XValue::Nodes(_)) => unreachable!("node sets go the other way"),
                    }),
                })
        };
        let not_nodes =
            "The result is not a node set, and therefore cannot be converted to the desired type.";
        let value = match evaluated {
            Err(NotNodes) => return Err(self.type_error(format!("{failed}: {not_nodes}"))),
            Ok(Ok(value)) => {
                if kind > 3 {
                    return Err(self.type_error(format!("{failed}: {not_nodes}")));
                }
                value
            }
            Ok(Err(nodes)) => match kind {
                1..=3 => {
                    let document = self.document.borrow();
                    let evaluation = Evaluation::new(&document);
                    let set = XValue::Nodes(nodes);
                    match kind {
                        1 => ResultValue::Number(evaluation.to_number(&set)),
                        2 => ResultValue::String(evaluation.to_string(&set)),
                        _ => ResultValue::Boolean(Evaluation::to_boolean(&set)),
                    }
                }
                _ => ResultValue::Nodes(
                    nodes
                        .into_iter()
                        .map(|node| match node {
                            XNode::Node(node) => self.node_value(node),
                            XNode::Attribute(element, index) => {
                                self.attribute_value(element, index)
                            }
                        })
                        .collect(),
                ),
            },
        };
        let kind = match (kind, &value) {
            (0, ResultValue::Number(_)) => 1,
            (0, ResultValue::String(_)) => 2,
            (0, ResultValue::Boolean(_)) => 3,
            (0, ResultValue::Nodes(_)) => 4,
            (kind, _) => kind,
        };
        let version = self.document.borrow().version();
        let prototype = self.intrinsics.xpath.result.clone();
        Ok(Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::XPath(Rc::new(XPathObject::Result(RefCell::new(XPathResult {
                kind,
                value,
                cursor: 0,
                version,
            })))),
        ))))
    }

    fn new_xpath_expression(&self, expression: Rc<Compiled>) -> Value {
        let prototype = self.intrinsics.xpath.expression.clone();
        Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::XPath(Rc::new(XPathObject::Expression(expression))),
        )))
    }

    // document.evaluate, document.createExpression and document.createNSResolver, and the
    // same three on an XPathEvaluator
    pub(super) fn call_xpath_evaluator(
        &mut self,
        member: XPathMember,
        arguments: &[Value],
        interface: &str,
    ) -> Result<Value, ScriptError> {
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(Value::Undefined);
        match member {
            XPathMember::Evaluate => {
                let failed = format!("Failed to execute 'evaluate' on '{interface}'");
                let text = self.coerce_string(argument(0))?;
                if !matches!(argument(1).resolve(), Value::Element(_) | Value::Document) {
                    return Err(
                        self.type_error(format!("{failed}: parameter 2 is not of type 'Node'."))
                    );
                }
                let expression = self.compile_xpath(&text, &argument(2), &failed)?;
                self.run_xpath(&expression, &argument(1), argument(3), &failed)
            }
            XPathMember::CreateExpression => {
                let failed = format!("Failed to execute 'createExpression' on '{interface}'");
                let text = self.coerce_string(argument(0))?;
                let expression = self.compile_xpath(&text, &argument(1), &failed)?;
                Ok(self.new_xpath_expression(expression))
            }
            _ => Ok(argument(0)),
        }
    }

    pub(super) fn construct_xpath_evaluator(
        &mut self,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        if matches!(new_target, Value::Undefined) {
            return Err(self.type_error("Failed to construct 'XPathEvaluator': Please use the 'new' operator, this DOM object constructor cannot be called as a function."));
        }
        let prototype = self.prototype_from_constructor(&new_target)?;
        Ok(Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::XPath(Rc::new(XPathObject::Evaluator)),
        ))))
    }

    pub(super) fn call_xpath(
        &mut self,
        member: XPathMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let found = Self::xpath_object(&this);
        match member {
            XPathMember::Constructor => self.construct_xpath_evaluator(Value::Undefined),
            XPathMember::Evaluate
            | XPathMember::CreateExpression
            | XPathMember::CreateNsResolver => {
                if !matches!(found.as_deref(), Some(XPathObject::Evaluator)) {
                    return Err(self.type_error("Illegal invocation"));
                }
                self.call_xpath_evaluator(member, &arguments, "XPathEvaluator")
            }
            XPathMember::ExpressionEvaluate => {
                let Some(XPathObject::Expression(expression)) = found.as_deref() else {
                    return Err(self.type_error("Illegal invocation"));
                };
                let context = arguments.first().cloned().unwrap_or(Value::Undefined);
                let kind = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                let failed = "Failed to execute 'evaluate' on 'XPathExpression'";
                if !matches!(context.resolve(), Value::Element(_) | Value::Document) {
                    return Err(
                        self.type_error(format!("{failed}: parameter 1 is not of type 'Node'."))
                    );
                }
                self.run_xpath(&expression.clone(), &context, kind, failed)
            }
            _ => {
                let Some(XPathObject::Result(result)) = found.as_deref() else {
                    return Err(self.type_error("Illegal invocation"));
                };
                self.call_xpath_result(member, result, arguments)
            }
        }
    }

    fn call_xpath_result(
        &mut self,
        member: XPathMember,
        result: &RefCell<XPathResult>,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let kind = result.borrow().kind;
        let stale =
            matches!(kind, 4 | 5) && result.borrow().version != self.document.borrow().version();
        let found = {
            let result = result.borrow();
            match (member, &result.value) {
                (XPathMember::ResultType, _) => Some(Value::Number(f64::from(kind))),
                (XPathMember::InvalidIteratorState, _) => Some(Value::Bool(stale)),
                (XPathMember::NumberValue, ResultValue::Number(number)) => {
                    Some(Value::Number(*number))
                }
                (XPathMember::StringValue, ResultValue::String(text)) => {
                    Some(Value::String(text.as_str().into()))
                }
                (XPathMember::BooleanValue, ResultValue::Boolean(value)) => {
                    Some(Value::Bool(*value))
                }
                (XPathMember::SingleNodeValue, ResultValue::Nodes(nodes))
                    if matches!(kind, 8 | 9) =>
                {
                    Some(nodes.first().cloned().unwrap_or(Value::Null))
                }
                (XPathMember::SnapshotLength, ResultValue::Nodes(nodes))
                    if matches!(kind, 6 | 7) =>
                {
                    Some(Value::Number(nodes.len() as f64))
                }
                (XPathMember::SnapshotItem, ResultValue::Nodes(_)) if matches!(kind, 6 | 7) => {
                    Some(Value::Undefined)
                }
                (XPathMember::IterateNext, ResultValue::Nodes(_)) if matches!(kind, 4 | 5) => {
                    Some(Value::Undefined)
                }
                _ => None,
            }
        };
        let Some(found) = found else {
            let (what, wanted) = match member {
                XPathMember::NumberValue => ("read the 'numberValue' property from", "a number"),
                XPathMember::StringValue => ("read the 'stringValue' property from", "a string"),
                XPathMember::BooleanValue => ("read the 'booleanValue' property from", "a boolean"),
                XPathMember::SingleNodeValue => {
                    ("read the 'singleNodeValue' property from", "a single node")
                }
                XPathMember::SnapshotLength => {
                    ("read the 'snapshotLength' property from", "a snapshot")
                }
                XPathMember::SnapshotItem => ("execute 'snapshotItem' on", "a snapshot"),
                _ => ("execute 'iterateNext' on", "an iterator"),
            };
            return Err(self.type_error(format!(
                "Failed to {what} 'XPathResult': The result type is not {wanted}."
            )));
        };
        match member {
            XPathMember::SnapshotItem => {
                let index =
                    self.coerce_number(arguments.first().cloned().unwrap_or(Value::Undefined))?;
                let index = if index.is_finite() {
                    index.trunc().rem_euclid(4294967296.0) as usize
                } else {
                    0
                };
                let result = result.borrow();
                let ResultValue::Nodes(nodes) = &result.value else {
                    unreachable!("a snapshot holds nodes")
                };
                Ok(nodes.get(index).cloned().unwrap_or(Value::Null))
            }
            XPathMember::IterateNext => {
                if stale {
                    return Err(self.throw_value(dom_exception(
                        "InvalidStateError",
                        "Failed to execute 'iterateNext' on 'XPathResult': The document has mutated since the result was returned.",
                    )));
                }
                let mut result = result.borrow_mut();
                let next = match &result.value {
                    ResultValue::Nodes(nodes) => nodes.get(result.cursor).cloned(),
                    _ => None,
                };
                result.cursor += 1;
                Ok(next.unwrap_or(Value::Null))
            }
            _ => Ok(found),
        }
    }

    // Node.lookupNamespaceURI: the namespace a prefix names where the node sits
    pub(super) fn lookup_namespace(&self, node: NodeId, prefix: Option<&str>) -> Value {
        match prefix {
            Some("xml") => return Value::String("http://www.w3.org/XML/1998/namespace".into()),
            Some("xmlns") => return Value::String("http://www.w3.org/2000/xmlns/".into()),
            _ => {}
        }
        let document = self.document.borrow();
        let mut at = match document.kind(node) {
            Some(NodeKind::Document) => document
                .children(node)
                .iter()
                .copied()
                .find(|&child| document.element(child).is_some()),
            Some(NodeKind::Element(_)) => Some(node),
            Some(NodeKind::Fragment) => None,
            _ => document.parent(node),
        };
        while let Some(element_id) = at {
            let Some(element) = document.element(element_id) else {
                at = document.parent(element_id);
                continue;
            };
            let declared = match prefix {
                None => {
                    return Value::String(element.namespace.uri().into());
                }
                Some(prefix) => element.attrs.get(&format!("xmlns:{prefix}")),
            };
            if let Some(uri) = declared {
                return if uri.is_empty() {
                    Value::Null
                } else {
                    Value::String(uri.as_str().into())
                };
            }
            at = document.parent(element_id);
        }
        Value::Null
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(source: &str) -> String {
        let document = crate::dom::Document::parse(
            r#"<div id="root"><p id="a" class="x">one <b>bold</b> two</p><p id="b" data-n="3">three</p><!--note--><ul><li>1</li><li>2</li><li>3</li><li>4</li></ul><svg id="s"><circle r="1"/></svg><span id="c" data-n="10">  spaced   out  </span></div><output id="out"></output>"#,
        )
        .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(&format!(
                "document.getElementById('out').textContent = (() => {{ {source} }})();"
            ))
            .unwrap_or_else(|error| panic!("{source}: {error:?}"));
        let out = document.borrow().find_element_by_id("out").unwrap();
        document
            .borrow()
            .element_text_content(out)
            .unwrap_or_default()
    }

    #[test]
    fn location_paths_select_nodes_in_document_order() {
        assert_eq!(
            run(r##"
                const name = (n) => n.nodeType === 1 ? n.localName + (n.id ? "#" + n.id : "") : n.nodeType === 3 ? "t:" + n.data.trim() : n.nodeType === 8 ? "c:" + n.data : "@" + n.name;
                const all = (expr, context = document) => {
                    const result = document.evaluate(expr, context, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
                    const out = [];
                    for (let i = 0; i < result.snapshotLength; i++) out.push(name(result.snapshotItem(i)));
                    return out.join(",");
                };
                return [
                    all("//P"), all("//p[@class='x']"), all("//p/@id"), all("//li[last()]/preceding-sibling::li[1]"),
                    all("(//li)[2] | //p[1]"), all("//b/ancestor::*[2]"), all("//ul/preceding-sibling::node()[1]"),
                    all("//li[3]/preceding::li"), all("//b/following::li[1]"), all("//p[1]//text()"),
                    all("//svg"), all("//*[local-name()='circle']"), all("id('a b')"), all("//li[. > 2 and . < 4]"),
                    all("//*[@data-n > 5]"), all("..", document.getElementById("a")),
                ].join(" | ");
            "##),
            "p#a,p#b | p#a | @id,@id | li | p#a,li | div#root | c:note | li,li | li | t:one,t:bold,t:two |  | circle | p#a,p#b | li | span#c | div#root"
        );
    }

    #[test]
    fn values_follow_xpath_conversions_and_functions() {
        assert_eq!(
            run(r##"
                const value = (expr) => {
                    const result = document.evaluate(expr, document, null, XPathResult.ANY_TYPE, null);
                    return result.resultType === 1 ? result.numberValue : result.resultType === 2 ? JSON.stringify(result.stringValue) : result.booleanValue;
                };
                return [
                    "count(//li)", "sum(//@data-n)", "string(//p[1])", "normalize-space(//span)", "concat('a', 1, true())",
                    "substring('12345', 1.5, 2.6)", "substring-before('1999/04/01', '/')", "substring-after('1999/04/01', '/')",
                    "translate('--aaa--', 'abc-', 'ABC')", "1 div 0", "-5 mod 2", "round(-2.5)", "number('1e3')",
                    "string(1 div 3)", "string(1000000000000000000000)", "string(1234567)", "string(0.000001)", "string(0.0000001)",
                    "//li = 2", "//li = //p", "true() = 'x'", "'a' < 'b'", "namespace-uri(//p)", "name(//@data-n)", "$x",
                    "2 + 3 * 4", "- - 3", "//li[1] < //li[2]",
                ].map(value).join(" ");
            "##),
            "4 13 \"one bold two\" \"spaced out\" \"a1true\" \"234\" \"1999\" \"04/01\" \"AAA\" Infinity -1 -2 NaN \"0.333333\" \"1.00000e+21\" \"1.23457e+6\" \"0.000001\" \"1.00000e-7\" true false true false \"http://www.w3.org/1999/xhtml\" \"data-n\" \"\" 14 3 true"
        );
    }

    #[test]
    fn results_expressions_and_errors_behave_like_the_dom_interfaces() {
        assert_eq!(
            run(r##"
                const out = [];
                const iterator = document.evaluate("//li", document, null, XPathResult.ORDERED_NODE_ITERATOR_TYPE, null);
                out.push(iterator.iterateNext().textContent, iterator.iterateNext().textContent);
                document.body.appendChild(document.createElement("i"));
                try { iterator.iterateNext(); } catch (error) { out.push(error.name); }
                out.push(iterator.invalidIteratorState);
                const first = document.evaluate("//li", document, null, 9, null);
                out.push(first.singleNodeValue.textContent, first.resultType);
                try { first.snapshotLength; } catch (error) { out.push(error.name); }
                try { document.evaluate("count(//li)", document, null, 7, null); } catch (error) { out.push(error.name); }
                out.push(document.evaluate("count(//li)", document, null, 2, null).stringValue);
                out.push(document.createExpression("count(li)").evaluate(document.querySelector("ul"), 1).numberValue);
                out.push(new XPathEvaluator().evaluate("string(//b)", document).stringValue);
                out.push(document.evaluate("//svg:circle", document, (p) => p === "svg" ? "http://www.w3.org/2000/svg" : null, 7, null).snapshotLength);
                out.push(document.createNSResolver(document.body) === document.body, document.body.lookupNamespaceURI(null));
                for (const bad of ["//", "//p[", "foo(", "1 +", "//x:p", "unknown()", "//p[1]]"]) {
                    try { document.evaluate(bad, document); out.push("ok"); } catch (error) { out.push(error.name); }
                }
                const attr = document.evaluate("//p/@id", document, null, 9, null).singleNodeValue;
                out.push(attr.nodeName, attr.value, attr.ownerElement.id, attr.nodeType);
                return out.join(" ");
            "##),
            "1 2 InvalidStateError true 1 9 TypeError TypeError 4 4 bold 1 true http://www.w3.org/1999/xhtml SyntaxError SyntaxError SyntaxError SyntaxError NamespaceError SyntaxError SyntaxError id a a 2"
        );
    }
}
