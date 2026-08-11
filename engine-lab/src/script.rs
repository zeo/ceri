use crate::dom::{Document, NodePath};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

type EnvironmentRef = Rc<RefCell<Environment>>;

#[derive(Clone, Debug, PartialEq)]
pub struct ScriptError {
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "script error at byte {}: {}",
            self.offset, self.message
        )
    }
}

impl std::error::Error for ScriptError {}

#[derive(Clone)]
enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Element(NodePath),
    Style(NodePath),
    Document,
    Window,
    Console,
    Function(Rc<Function>),
    Builtin(Builtin),
}

#[derive(Clone)]
enum Builtin {
    DocumentGetElementById,
    DocumentQuerySelector,
    ConsoleLog,
    ElementAddEventListener(NodePath),
    ElementClick(NodePath),
    ElementSetAttribute(NodePath),
    ElementGetAttribute(NodePath),
}

struct Function {
    params: Vec<String>,
    body: Vec<Stmt>,
    environment: EnvironmentRef,
}

#[derive(Default)]
struct Environment {
    values: HashMap<String, Value>,
    parent: Option<EnvironmentRef>,
}

#[derive(Clone, Debug, PartialEq)]
enum Expr {
    Literal(Literal),
    Identifier(String),
    Unary {
        operator: UnaryOperator,
        expression: Box<Expr>,
    },
    Binary {
        left: Box<Expr>,
        operator: BinaryOperator,
        right: Box<Expr>,
    },
    Assignment {
        target: Box<Expr>,
        expression: Box<Expr>,
    },
    Member {
        object: Box<Expr>,
        property: String,
    },
    Call {
        callee: Box<Expr>,
        arguments: Vec<Expr>,
    },
    Function {
        params: Vec<String>,
        body: Vec<Stmt>,
    },
}

#[derive(Clone, Debug, PartialEq)]
enum Literal {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
}

#[derive(Clone, Debug, PartialEq)]
enum Stmt {
    Let {
        name: String,
        initializer: Option<Expr>,
    },
    Expression(Expr),
    Block(Vec<Stmt>),
    If {
        condition: Expr,
        consequent: Box<Stmt>,
        alternate: Option<Box<Stmt>>,
    },
    Return(Option<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnaryOperator {
    Not,
    Negative,
    Positive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BinaryOperator {
    Add,
    Subtract,
    Multiply,
    Divide,
    Equal,
    NotEqual,
    StrictEqual,
    StrictNotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    And,
    Or,
}

pub struct ScriptRuntime {
    document: Rc<RefCell<Document>>,
    global: EnvironmentRef,
    listeners: HashMap<NodePath, Vec<EventListener>>,
    logs: Vec<String>,
}

#[derive(Clone)]
struct EventListener {
    event: String,
    callback: Value,
}

impl ScriptRuntime {
    pub fn new(document: Rc<RefCell<Document>>) -> Self {
        let global = Rc::new(RefCell::new(Environment::default()));
        {
            let mut values = global.borrow_mut();
            values
                .values
                .insert("document".to_string(), Value::Document);
            values.values.insert("window".to_string(), Value::Window);
            values.values.insert("console".to_string(), Value::Console);
        }
        Self {
            document,
            global,
            listeners: HashMap::new(),
            logs: Vec::new(),
        }
    }

    pub fn execute_scripts(&mut self) -> Result<(), ScriptError> {
        let scripts = self.document.borrow().script_text();
        for script in scripts {
            self.execute(&script)?;
        }
        Ok(())
    }

    pub fn execute(&mut self, source: &str) -> Result<(), ScriptError> {
        let program = Parser::new(source)?.parse_program()?;
        match self.execute_statements(&program, self.global.clone())? {
            Completion::Normal | Completion::Return(_) => Ok(()),
        }
    }

    pub fn click_id(&mut self, id: &str) -> Result<bool, ScriptError> {
        let Some(path) = self.document.borrow().find_element_by_id(id) else {
            return Ok(false);
        };
        self.dispatch_event(path, "click")
    }

    pub fn logs(&self) -> &[String] {
        &self.logs
    }

    pub fn document(&self) -> Rc<RefCell<Document>> {
        self.document.clone()
    }

    fn execute_statements(
        &mut self,
        statements: &[Stmt],
        environment: EnvironmentRef,
    ) -> Result<Completion, ScriptError> {
        for statement in statements {
            let completion = self.execute_statement(statement, environment.clone())?;
            if !matches!(completion, Completion::Normal) {
                return Ok(completion);
            }
        }
        Ok(Completion::Normal)
    }

    fn execute_statement(
        &mut self,
        statement: &Stmt,
        environment: EnvironmentRef,
    ) -> Result<Completion, ScriptError> {
        match statement {
            Stmt::Let { name, initializer } => {
                let value = initializer
                    .as_ref()
                    .map(|expression| self.evaluate(expression, environment.clone()))
                    .transpose()?
                    .unwrap_or(Value::Undefined);
                environment.borrow_mut().values.insert(name.clone(), value);
                Ok(Completion::Normal)
            }
            Stmt::Expression(expression) => {
                self.evaluate(expression, environment)?;
                Ok(Completion::Normal)
            }
            Stmt::Block(statements) => {
                let block = Rc::new(RefCell::new(Environment {
                    values: HashMap::new(),
                    parent: Some(environment),
                }));
                self.execute_statements(statements, block)
            }
            Stmt::If {
                condition,
                consequent,
                alternate,
            } => {
                if self.evaluate(condition, environment.clone())?.truthy() {
                    self.execute_statement(consequent, environment)
                } else if let Some(alternate) = alternate {
                    self.execute_statement(alternate, environment)
                } else {
                    Ok(Completion::Normal)
                }
            }
            Stmt::Return(expression) => Ok(Completion::Return(
                expression
                    .as_ref()
                    .map(|expression| self.evaluate(expression, environment))
                    .transpose()?
                    .unwrap_or(Value::Undefined),
            )),
        }
    }

    fn evaluate(
        &mut self,
        expression: &Expr,
        environment: EnvironmentRef,
    ) -> Result<Value, ScriptError> {
        match expression {
            Expr::Literal(literal) => Ok(literal.to_value()),
            Expr::Identifier(name) => environment
                .borrow()
                .get(name)
                .ok_or_else(|| runtime_error(format!("unknown identifier {name}"))),
            Expr::Unary {
                operator,
                expression,
            } => {
                let value = self.evaluate(expression, environment)?;
                match operator {
                    UnaryOperator::Not => Ok(Value::Bool(!value.truthy())),
                    UnaryOperator::Negative => Ok(Value::Number(-value.number()?)),
                    UnaryOperator::Positive => Ok(Value::Number(value.number()?)),
                }
            }
            Expr::Binary {
                left,
                operator,
                right,
            } => {
                let left = self.evaluate(left, environment.clone())?;
                if *operator == BinaryOperator::And && !left.truthy() {
                    return Ok(left);
                }
                if *operator == BinaryOperator::Or && left.truthy() {
                    return Ok(left);
                }
                let right = self.evaluate(right, environment)?;
                evaluate_binary(*operator, left, right)
            }
            Expr::Assignment { target, expression } => {
                let value = self.evaluate(expression, environment.clone())?;
                self.assign(target, value.clone(), environment)?;
                Ok(value)
            }
            Expr::Member { object, property } => {
                let object = self.evaluate(object, environment)?;
                self.member(object, property)
            }
            Expr::Call { callee, arguments } => {
                let callee = self.evaluate(callee, environment.clone())?;
                let arguments = arguments
                    .iter()
                    .map(|argument| self.evaluate(argument, environment.clone()))
                    .collect::<Result<Vec<_>, _>>()?;
                self.call(callee, arguments)
            }
            Expr::Function { params, body } => Ok(Value::Function(Rc::new(Function {
                params: params.clone(),
                body: body.clone(),
                environment,
            }))),
        }
    }

    fn assign(
        &mut self,
        target: &Expr,
        value: Value,
        environment: EnvironmentRef,
    ) -> Result<(), ScriptError> {
        match target {
            Expr::Identifier(name) => {
                if !environment.borrow_mut().assign(name, value.clone()) {
                    environment.borrow_mut().values.insert(name.clone(), value);
                }
                Ok(())
            }
            Expr::Member { object, property } => {
                let object = self.evaluate(object, environment)?;
                self.assign_member(object, property, value)
            }
            _ => Err(runtime_error("assignment target is not writable")),
        }
    }

    fn member(&self, object: Value, property: &str) -> Result<Value, ScriptError> {
        match object {
            Value::Document => match property {
                "getElementById" => Ok(Value::Builtin(Builtin::DocumentGetElementById)),
                "querySelector" => Ok(Value::Builtin(Builtin::DocumentQuerySelector)),
                _ => Err(runtime_error(format!(
                    "document has no property {property}"
                ))),
            },
            Value::Window => match property {
                "document" => Ok(Value::Document),
                "console" => Ok(Value::Console),
                _ => Err(runtime_error(format!("window has no property {property}"))),
            },
            Value::Console if property == "log" => Ok(Value::Builtin(Builtin::ConsoleLog)),
            Value::Element(path) => {
                let value = match property {
                    "textContent" => self
                        .document
                        .borrow()
                        .element_text_content(&path)
                        .map(Value::String)
                        .unwrap_or(Value::Undefined),
                    "className" => self
                        .document
                        .borrow()
                        .element_attribute(&path, "class")
                        .map(Value::String)
                        .unwrap_or_else(|| Value::String(String::new())),
                    "id" => self
                        .document
                        .borrow()
                        .element_attribute(&path, "id")
                        .map(Value::String)
                        .unwrap_or_else(|| Value::String(String::new())),
                    "style" => Value::Style(path),
                    "addEventListener" => Value::Builtin(Builtin::ElementAddEventListener(path)),
                    "click" => Value::Builtin(Builtin::ElementClick(path)),
                    "setAttribute" => Value::Builtin(Builtin::ElementSetAttribute(path)),
                    "getAttribute" => Value::Builtin(Builtin::ElementGetAttribute(path)),
                    _ => return Err(runtime_error(format!("element has no property {property}"))),
                };
                Ok(value)
            }
            Value::Style(path) => Ok(self
                .document
                .borrow()
                .style_property(&path, property)
                .map(Value::String)
                .unwrap_or_else(|| Value::String(String::new()))),
            _ => Err(runtime_error(format!("value has no property {property}"))),
        }
    }

    fn assign_member(
        &mut self,
        object: Value,
        property: &str,
        value: Value,
    ) -> Result<(), ScriptError> {
        let value = value.string_value();
        let updated = match object {
            Value::Element(path) => match property {
                "textContent" => self
                    .document
                    .borrow_mut()
                    .set_element_text_content(&path, value),
                "className" => self
                    .document
                    .borrow_mut()
                    .set_element_attribute(&path, "class", value),
                "id" => self
                    .document
                    .borrow_mut()
                    .set_element_attribute(&path, "id", value),
                _ => false,
            },
            Value::Style(path) => self
                .document
                .borrow_mut()
                .set_style_property(&path, property, value),
            _ => false,
        };
        updated
            .then_some(())
            .ok_or_else(|| runtime_error(format!("property {property} is not writable")))
    }

    fn call(&mut self, callee: Value, arguments: Vec<Value>) -> Result<Value, ScriptError> {
        match callee {
            Value::Function(function) => {
                let environment = Rc::new(RefCell::new(Environment {
                    values: HashMap::new(),
                    parent: Some(function.environment.clone()),
                }));
                for (index, parameter) in function.params.iter().enumerate() {
                    environment.borrow_mut().values.insert(
                        parameter.clone(),
                        arguments.get(index).cloned().unwrap_or(Value::Undefined),
                    );
                }
                match self.execute_statements(&function.body, environment)? {
                    Completion::Normal => Ok(Value::Undefined),
                    Completion::Return(value) => Ok(value),
                }
            }
            Value::Builtin(builtin) => self.call_builtin(builtin, arguments),
            _ => Err(runtime_error("value is not callable")),
        }
    }

    fn call_builtin(
        &mut self,
        builtin: Builtin,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        match builtin {
            Builtin::DocumentGetElementById => {
                let id = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                Ok(self
                    .document
                    .borrow()
                    .find_element_by_id(&id)
                    .map(Value::Element)
                    .unwrap_or(Value::Null))
            }
            Builtin::DocumentQuerySelector => {
                let selector = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                Ok(self
                    .document
                    .borrow()
                    .find_element_by_selector(&selector)
                    .map(Value::Element)
                    .unwrap_or(Value::Null))
            }
            Builtin::ConsoleLog => {
                self.logs.push(
                    arguments
                        .iter()
                        .map(Value::display_string)
                        .collect::<Vec<_>>()
                        .join(" "),
                );
                Ok(Value::Undefined)
            }
            Builtin::ElementAddEventListener(path) => {
                let event = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let callback = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                if !matches!(callback, Value::Function(_)) {
                    return Err(runtime_error("event listener must be a function"));
                }
                self.listeners
                    .entry(path)
                    .or_default()
                    .push(EventListener { event, callback });
                Ok(Value::Undefined)
            }
            Builtin::ElementClick(path) => {
                self.dispatch_event(path, "click")?;
                Ok(Value::Undefined)
            }
            Builtin::ElementSetAttribute(path) => {
                let name = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let value = arguments
                    .get(1)
                    .map(Value::string_value)
                    .unwrap_or_default();
                self.document
                    .borrow_mut()
                    .set_element_attribute(&path, &name, value);
                Ok(Value::Undefined)
            }
            Builtin::ElementGetAttribute(path) => {
                let name = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                Ok(self
                    .document
                    .borrow()
                    .element_attribute(&path, &name)
                    .map(Value::String)
                    .unwrap_or(Value::Null))
            }
        }
    }

    fn dispatch_event(&mut self, path: NodePath, event: &str) -> Result<bool, ScriptError> {
        let callbacks = self
            .listeners
            .get(&path)
            .into_iter()
            .flatten()
            .filter(|listener| listener.event == event)
            .map(|listener| listener.callback.clone())
            .collect::<Vec<_>>();
        let had_callbacks = !callbacks.is_empty();
        for callback in callbacks {
            self.call(callback, vec![Value::String(event.to_string())])?;
        }
        Ok(had_callbacks)
    }
}

enum Completion {
    Normal,
    Return(Value),
}

impl Environment {
    fn get(&self, name: &str) -> Option<Value> {
        self.values.get(name).cloned().or_else(|| {
            self.parent
                .as_ref()
                .and_then(|parent| parent.borrow().get(name))
        })
    }

    fn assign(&mut self, name: &str, value: Value) -> bool {
        if self.values.contains_key(name) {
            self.values.insert(name.to_string(), value);
            true
        } else if let Some(parent) = &self.parent {
            parent.borrow_mut().assign(name, value)
        } else {
            false
        }
    }
}

impl Literal {
    fn to_value(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(value) => Value::Bool(*value),
            Self::Number(value) => Value::Number(*value),
            Self::String(value) => Value::String(value.clone()),
        }
    }
}

impl Value {
    fn truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Bool(value) => *value,
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::String(value) => !value.is_empty(),
            _ => true,
        }
    }

    fn number(&self) -> Result<f64, ScriptError> {
        match self {
            Self::Number(value) => Ok(*value),
            Self::String(value) => value
                .parse()
                .map_err(|_| runtime_error("value is not numeric")),
            Self::Bool(value) => Ok(f64::from(*value as u8)),
            _ => Err(runtime_error("value is not numeric")),
        }
    }

    fn string_value(&self) -> String {
        match self {
            Self::Undefined => "undefined".to_string(),
            Self::Null => "null".to_string(),
            Self::Bool(value) => value.to_string(),
            Self::Number(value) if value.fract() == 0.0 => format!("{value:.0}"),
            Self::Number(value) => value.to_string(),
            Self::String(value) => value.clone(),
            Self::Element(_) => "[element]".to_string(),
            Self::Style(_) => "[style]".to_string(),
            Self::Document => "[document]".to_string(),
            Self::Window => "[window]".to_string(),
            Self::Console => "[console]".to_string(),
            Self::Function(_) => "[function]".to_string(),
            Self::Builtin(_) => "[function]".to_string(),
        }
    }

    fn display_string(&self) -> String {
        self.string_value()
    }
}

fn evaluate_binary(
    operator: BinaryOperator,
    left: Value,
    right: Value,
) -> Result<Value, ScriptError> {
    match operator {
        BinaryOperator::Add => {
            if matches!(left, Value::String(_)) || matches!(right, Value::String(_)) {
                Ok(Value::String(format!(
                    "{}{}",
                    left.string_value(),
                    right.string_value()
                )))
            } else {
                Ok(Value::Number(left.number()? + right.number()?))
            }
        }
        BinaryOperator::Subtract => Ok(Value::Number(left.number()? - right.number()?)),
        BinaryOperator::Multiply => Ok(Value::Number(left.number()? * right.number()?)),
        BinaryOperator::Divide => Ok(Value::Number(left.number()? / right.number()?)),
        BinaryOperator::Equal | BinaryOperator::StrictEqual => {
            Ok(Value::Bool(values_equal(&left, &right)))
        }
        BinaryOperator::NotEqual | BinaryOperator::StrictNotEqual => {
            Ok(Value::Bool(!values_equal(&left, &right)))
        }
        BinaryOperator::Less => Ok(Value::Bool(left.number()? < right.number()?)),
        BinaryOperator::LessEqual => Ok(Value::Bool(left.number()? <= right.number()?)),
        BinaryOperator::Greater => Ok(Value::Bool(left.number()? > right.number()?)),
        BinaryOperator::GreaterEqual => Ok(Value::Bool(left.number()? >= right.number()?)),
        BinaryOperator::And | BinaryOperator::Or => unreachable!("logical operators short-circuit"),
    }
}

fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Bool(left), Value::Bool(right)) => left == right,
        (Value::Number(left), Value::Number(right)) => left == right,
        (Value::String(left), Value::String(right)) => left == right,
        (Value::Element(left), Value::Element(right))
        | (Value::Style(left), Value::Style(right)) => left == right,
        _ => false,
    }
}

fn runtime_error(message: impl Into<String>) -> ScriptError {
    ScriptError {
        offset: 0,
        message: message.into(),
    }
}

struct Parser {
    tokens: Vec<Token>,
    index: usize,
}

impl Parser {
    fn new(source: &str) -> Result<Self, ScriptError> {
        Ok(Self {
            tokens: tokenize(source)?,
            index: 0,
        })
    }

    fn parse_program(&mut self) -> Result<Vec<Stmt>, ScriptError> {
        let mut statements = Vec::new();
        while !self.is(TokenKind::Eof) {
            statements.push(self.parse_statement()?);
        }
        Ok(statements)
    }

    fn parse_statement(&mut self) -> Result<Stmt, ScriptError> {
        if self.take(TokenKind::Semicolon) {
            return Ok(Stmt::Block(Vec::new()));
        }
        if self.take(TokenKind::LBrace) {
            let statements = self.parse_block_contents()?;
            return Ok(Stmt::Block(statements));
        }
        if self.take(TokenKind::Let) || self.take(TokenKind::Const) {
            let name = self.expect_identifier("variable name")?;
            let initializer = self
                .take(TokenKind::Assign)
                .then(|| self.parse_expression())
                .transpose()?;
            self.take(TokenKind::Semicolon);
            return Ok(Stmt::Let { name, initializer });
        }
        if self.take(TokenKind::If) {
            self.expect(TokenKind::LParen, "if condition")?;
            let condition = self.parse_expression()?;
            self.expect(TokenKind::RParen, "if condition")?;
            let consequent = Box::new(self.parse_statement()?);
            let alternate = self
                .take(TokenKind::Else)
                .then(|| self.parse_statement().map(Box::new))
                .transpose()?;
            return Ok(Stmt::If {
                condition,
                consequent,
                alternate,
            });
        }
        if self.take(TokenKind::Return) {
            let expression = if self.is(TokenKind::Semicolon) || self.is(TokenKind::RBrace) {
                None
            } else {
                Some(self.parse_expression()?)
            };
            self.take(TokenKind::Semicolon);
            return Ok(Stmt::Return(expression));
        }
        if self.is(TokenKind::Function) && self.is_next_identifier() {
            self.advance();
            let name = self.expect_identifier("function name")?;
            let function = self.parse_function_after_name()?;
            self.take(TokenKind::Semicolon);
            return Ok(Stmt::Let {
                name,
                initializer: Some(function),
            });
        }
        let expression = self.parse_expression()?;
        self.take(TokenKind::Semicolon);
        Ok(Stmt::Expression(expression))
    }

    fn parse_block_contents(&mut self) -> Result<Vec<Stmt>, ScriptError> {
        let mut statements = Vec::new();
        while !self.is(TokenKind::RBrace) && !self.is(TokenKind::Eof) {
            statements.push(self.parse_statement()?);
        }
        self.expect(TokenKind::RBrace, "block")?;
        Ok(statements)
    }

    fn parse_expression(&mut self) -> Result<Expr, ScriptError> {
        self.parse_assignment()
    }

    fn parse_assignment(&mut self) -> Result<Expr, ScriptError> {
        let left = self.parse_or()?;
        if self.take(TokenKind::Assign) {
            let expression = self.parse_assignment()?;
            Ok(Expr::Assignment {
                target: Box::new(left),
                expression: Box::new(expression),
            })
        } else {
            Ok(left)
        }
    }

    fn parse_or(&mut self) -> Result<Expr, ScriptError> {
        self.parse_binary(Self::parse_and, &[TokenKind::OrOr], &[BinaryOperator::Or])
    }

    fn parse_and(&mut self) -> Result<Expr, ScriptError> {
        self.parse_binary(
            Self::parse_equality,
            &[TokenKind::AndAnd],
            &[BinaryOperator::And],
        )
    }

    fn parse_equality(&mut self) -> Result<Expr, ScriptError> {
        self.parse_binary(
            Self::parse_comparison,
            &[
                TokenKind::EqEq,
                TokenKind::NotEq,
                TokenKind::StrictEq,
                TokenKind::StrictNotEq,
            ],
            &[
                BinaryOperator::Equal,
                BinaryOperator::NotEqual,
                BinaryOperator::StrictEqual,
                BinaryOperator::StrictNotEqual,
            ],
        )
    }

    fn parse_comparison(&mut self) -> Result<Expr, ScriptError> {
        self.parse_binary(
            Self::parse_term,
            &[
                TokenKind::Less,
                TokenKind::LessEqual,
                TokenKind::Greater,
                TokenKind::GreaterEqual,
            ],
            &[
                BinaryOperator::Less,
                BinaryOperator::LessEqual,
                BinaryOperator::Greater,
                BinaryOperator::GreaterEqual,
            ],
        )
    }

    fn parse_term(&mut self) -> Result<Expr, ScriptError> {
        self.parse_binary(
            Self::parse_factor,
            &[TokenKind::Plus, TokenKind::Minus],
            &[BinaryOperator::Add, BinaryOperator::Subtract],
        )
    }

    fn parse_factor(&mut self) -> Result<Expr, ScriptError> {
        self.parse_binary(
            Self::parse_unary,
            &[TokenKind::Star, TokenKind::Slash],
            &[BinaryOperator::Multiply, BinaryOperator::Divide],
        )
    }

    fn parse_binary(
        &mut self,
        next: fn(&mut Self) -> Result<Expr, ScriptError>,
        tokens: &[TokenKind],
        operators: &[BinaryOperator],
    ) -> Result<Expr, ScriptError> {
        let mut expression = next(self)?;
        loop {
            let Some(index) = tokens.iter().position(|token| self.is(token.clone())) else {
                break;
            };
            self.advance();
            let right = next(self)?;
            expression = Expr::Binary {
                left: Box::new(expression),
                operator: operators[index],
                right: Box::new(right),
            };
        }
        Ok(expression)
    }

    fn parse_unary(&mut self) -> Result<Expr, ScriptError> {
        let operator = if self.take(TokenKind::Bang) {
            Some(UnaryOperator::Not)
        } else if self.take(TokenKind::Minus) {
            Some(UnaryOperator::Negative)
        } else if self.take(TokenKind::Plus) {
            Some(UnaryOperator::Positive)
        } else {
            None
        };
        if let Some(operator) = operator {
            Ok(Expr::Unary {
                operator,
                expression: Box::new(self.parse_unary()?),
            })
        } else {
            self.parse_postfix()
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, ScriptError> {
        let mut expression = self.parse_primary()?;
        loop {
            if self.take(TokenKind::Dot) {
                let property = self.expect_identifier("property name")?;
                expression = Expr::Member {
                    object: Box::new(expression),
                    property,
                };
            } else if self.take(TokenKind::LParen) {
                let mut arguments = Vec::new();
                if !self.is(TokenKind::RParen) {
                    loop {
                        arguments.push(self.parse_expression()?);
                        if !self.take(TokenKind::Comma) {
                            break;
                        }
                    }
                }
                self.expect(TokenKind::RParen, "call")?;
                expression = Expr::Call {
                    callee: Box::new(expression),
                    arguments,
                };
            } else {
                break;
            }
        }
        Ok(expression)
    }

    fn parse_primary(&mut self) -> Result<Expr, ScriptError> {
        if let Some(function) = self.try_parse_arrow_function()? {
            return Ok(function);
        }
        let token = self.advance();
        match token.kind {
            TokenKind::Number(value) => Ok(Expr::Literal(Literal::Number(value))),
            TokenKind::String(value) => Ok(Expr::Literal(Literal::String(value))),
            TokenKind::True => Ok(Expr::Literal(Literal::Bool(true))),
            TokenKind::False => Ok(Expr::Literal(Literal::Bool(false))),
            TokenKind::Null => Ok(Expr::Literal(Literal::Null)),
            TokenKind::Identifier(name) => {
                if self.take(TokenKind::Arrow) {
                    Ok(self.parse_arrow_body(vec![name])?)
                } else {
                    Ok(Expr::Identifier(name))
                }
            }
            TokenKind::Function => self.parse_function_expression(),
            TokenKind::LParen => {
                let expression = self.parse_expression()?;
                self.expect(TokenKind::RParen, "parenthesized expression")?;
                Ok(expression)
            }
            other => Err(self.error(format!("unexpected token {other:?}"), token.offset)),
        }
    }

    fn try_parse_arrow_function(&mut self) -> Result<Option<Expr>, ScriptError> {
        if !self.is(TokenKind::LParen) {
            return Ok(None);
        }
        let saved = self.index;
        self.advance();
        let mut params = Vec::new();
        if !self.is(TokenKind::RParen) {
            loop {
                params.push(self.expect_identifier("arrow parameter")?);
                if !self.take(TokenKind::Comma) {
                    break;
                }
            }
        }
        if self.take(TokenKind::RParen) && self.take(TokenKind::Arrow) {
            return Ok(Some(self.parse_arrow_body(params)?));
        }
        self.index = saved;
        Ok(None)
    }

    fn parse_arrow_body(&mut self, params: Vec<String>) -> Result<Expr, ScriptError> {
        let body = if self.take(TokenKind::LBrace) {
            self.parse_block_contents()?
        } else {
            vec![Stmt::Return(Some(self.parse_expression()?))]
        };
        Ok(Expr::Function { params, body })
    }

    fn parse_function_expression(&mut self) -> Result<Expr, ScriptError> {
        if self.is_next_identifier() {
            self.advance();
        }
        let function = self.parse_function_after_name()?;
        let Expr::Function { params, body } = function else {
            unreachable!("function parser returns a function expression");
        };
        Ok(Expr::Function { params, body })
    }

    fn parse_function_after_name(&mut self) -> Result<Expr, ScriptError> {
        self.expect(TokenKind::LParen, "function parameters")?;
        let mut params = Vec::new();
        if !self.is(TokenKind::RParen) {
            loop {
                params.push(self.expect_identifier("function parameter")?);
                if !self.take(TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(TokenKind::RParen, "function parameters")?;
        self.expect(TokenKind::LBrace, "function body")?;
        let body = self.parse_block_contents()?;
        Ok(Expr::Function { params, body })
    }

    fn is_next_identifier(&self) -> bool {
        matches!(
            self.tokens.get(self.index + 1).map(|token| &token.kind),
            Some(TokenKind::Identifier(_))
        )
    }

    fn expect_identifier(&mut self, context: &str) -> Result<String, ScriptError> {
        let token = self.advance();
        match token.kind {
            TokenKind::Identifier(name) => Ok(name),
            _ => Err(self.error(format!("expected {context}"), token.offset)),
        }
    }

    fn expect(&mut self, expected: TokenKind, context: &str) -> Result<(), ScriptError> {
        if self.take(expected) {
            Ok(())
        } else {
            Err(self.error(format!("expected {context}"), self.peek().offset))
        }
    }

    fn take(&mut self, expected: TokenKind) -> bool {
        if self.is(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn is(&self, expected: TokenKind) -> bool {
        token_matches(&self.peek().kind, &expected)
    }

    fn peek(&self) -> &Token {
        self.tokens
            .get(self.index)
            .expect("token stream always has eof")
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        self.index += 1;
        token
    }

    fn error(&self, message: String, offset: usize) -> ScriptError {
        ScriptError { offset, message }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Token {
    kind: TokenKind,
    offset: usize,
}

#[derive(Clone, Debug, PartialEq)]
enum TokenKind {
    Eof,
    Identifier(String),
    Number(f64),
    String(String),
    True,
    False,
    Null,
    Let,
    Const,
    If,
    Else,
    Return,
    Function,
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Dot,
    Semicolon,
    Arrow,
    Assign,
    Plus,
    Minus,
    Star,
    Slash,
    Bang,
    EqEq,
    NotEq,
    StrictEq,
    StrictNotEq,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    AndAnd,
    OrOr,
}

fn tokenize(source: &str) -> Result<Vec<Token>, ScriptError> {
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while cursor < source.len() {
        let byte = source.as_bytes()[cursor];
        if byte.is_ascii_whitespace() {
            cursor += 1;
            continue;
        }
        if source[cursor..].starts_with("//") {
            cursor = source[cursor + 2..]
                .find('\n')
                .map(|offset| cursor + 2 + offset)
                .unwrap_or(source.len());
            continue;
        }
        if source[cursor..].starts_with("/*") {
            let Some(end) = source[cursor + 2..].find("*/") else {
                return Err(ScriptError {
                    offset: cursor,
                    message: "unterminated comment".to_string(),
                });
            };
            cursor += 2 + end + 2;
            continue;
        }
        if byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$') {
            let start = cursor;
            cursor += 1;
            while cursor < source.len()
                && (source.as_bytes()[cursor].is_ascii_alphanumeric()
                    || matches!(source.as_bytes()[cursor], b'_' | b'$'))
            {
                cursor += 1;
            }
            let identifier = &source[start..cursor];
            let kind = match identifier {
                "true" => TokenKind::True,
                "false" => TokenKind::False,
                "null" => TokenKind::Null,
                "let" => TokenKind::Let,
                "const" => TokenKind::Const,
                "if" => TokenKind::If,
                "else" => TokenKind::Else,
                "return" => TokenKind::Return,
                "function" => TokenKind::Function,
                _ => TokenKind::Identifier(identifier.to_string()),
            };
            tokens.push(Token {
                kind,
                offset: start,
            });
            continue;
        }
        if byte.is_ascii_digit() {
            let start = cursor;
            cursor += 1;
            while cursor < source.len()
                && (source.as_bytes()[cursor].is_ascii_digit() || source.as_bytes()[cursor] == b'.')
            {
                cursor += 1;
            }
            let number = source[start..cursor].parse().map_err(|_| ScriptError {
                offset: start,
                message: "invalid number".to_string(),
            })?;
            tokens.push(Token {
                kind: TokenKind::Number(number),
                offset: start,
            });
            continue;
        }
        if byte == b'\'' || byte == b'"' {
            let start = cursor;
            let quote = byte;
            cursor += 1;
            let mut value = String::new();
            while cursor < source.len() {
                let byte = source.as_bytes()[cursor];
                cursor += 1;
                if byte == quote {
                    break;
                }
                if byte == b'\\' {
                    let Some(escaped) = source.as_bytes().get(cursor).copied() else {
                        return Err(ScriptError {
                            offset: start,
                            message: "unterminated string".to_string(),
                        });
                    };
                    cursor += 1;
                    value.push(match escaped {
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'\\' => '\\',
                        b'\'' => '\'',
                        b'"' => '"',
                        other => other as char,
                    });
                } else {
                    value.push(byte as char);
                }
                if cursor == source.len() {
                    return Err(ScriptError {
                        offset: start,
                        message: "unterminated string".to_string(),
                    });
                }
            }
            if source.as_bytes().get(cursor.wrapping_sub(1)) != Some(&quote) {
                return Err(ScriptError {
                    offset: start,
                    message: "unterminated string".to_string(),
                });
            }
            tokens.push(Token {
                kind: TokenKind::String(value),
                offset: start,
            });
            continue;
        }
        let start = cursor;
        let (kind, width) = match source.as_bytes()[cursor..].as_ref() {
            [b'(', ..] => (TokenKind::LParen, 1),
            [b')', ..] => (TokenKind::RParen, 1),
            [b'{', ..] => (TokenKind::LBrace, 1),
            [b'}', ..] => (TokenKind::RBrace, 1),
            [b',', ..] => (TokenKind::Comma, 1),
            [b'.', ..] => (TokenKind::Dot, 1),
            [b';', ..] => (TokenKind::Semicolon, 1),
            [b'+', ..] => (TokenKind::Plus, 1),
            [b'-', ..] => (TokenKind::Minus, 1),
            [b'*', ..] => (TokenKind::Star, 1),
            [b'/', ..] => (TokenKind::Slash, 1),
            [b'!', b'=', b'=', ..] => (TokenKind::StrictNotEq, 3),
            [b'!', b'=', ..] => (TokenKind::NotEq, 2),
            [b'!', ..] => (TokenKind::Bang, 1),
            [b'=', b'=', b'=', ..] => (TokenKind::StrictEq, 3),
            [b'=', b'=', ..] => (TokenKind::EqEq, 2),
            [b'=', b'>', ..] => (TokenKind::Arrow, 2),
            [b'=', ..] => (TokenKind::Assign, 1),
            [b'<', b'=', ..] => (TokenKind::LessEqual, 2),
            [b'<', ..] => (TokenKind::Less, 1),
            [b'>', b'=', ..] => (TokenKind::GreaterEqual, 2),
            [b'>', ..] => (TokenKind::Greater, 1),
            [b'&', b'&', ..] => (TokenKind::AndAnd, 2),
            [b'|', b'|', ..] => (TokenKind::OrOr, 2),
            _ => {
                return Err(ScriptError {
                    offset: start,
                    message: format!(
                        "unsupported character {:?}",
                        source.as_bytes()[cursor] as char
                    ),
                });
            }
        };
        cursor += width;
        tokens.push(Token {
            kind,
            offset: start,
        });
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        offset: source.len(),
    });
    Ok(tokens)
}

fn token_matches(actual: &TokenKind, expected: &TokenKind) -> bool {
    match (actual, expected) {
        (TokenKind::Identifier(_), TokenKind::Identifier(_))
        | (TokenKind::Number(_), TokenKind::Number(_))
        | (TokenKind::String(_), TokenKind::String(_)) => true,
        _ => actual == expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(source: &str) -> Rc<RefCell<Document>> {
        Rc::new(RefCell::new(
            Document::parse(source).expect("document should parse"),
        ))
    }

    #[test]
    fn executes_dom_mutation_and_console_logging() {
        let document = document("<button id='go'>before</button><p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const button = document.getElementById("go");
                const status = document.getElementById("status");
                status.textContent = button.textContent + " changed";
                console.log(status.textContent);
                "##,
            )
            .expect("script should execute");
        let path = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&path).as_deref(),
            Some("before changed")
        );
        assert_eq!(runtime.logs(), &["before changed"]);
    }

    #[test]
    fn dispatches_an_event_listener_and_updates_style() {
        let document = document("<button id='go'>before</button><p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const button = document.getElementById("go");
                const status = document.getElementById("status");
                button.addEventListener("click", () => {
                    status.textContent = "clicked";
                    status.style.color = "#42d3b4";
                });
                "##,
            )
            .expect("listener should register");
        assert!(runtime.click_id("go").expect("click should dispatch"));
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("clicked")
        );
        assert_eq!(
            document
                .borrow()
                .style_property(&status, "color")
                .as_deref(),
            Some("#42d3b4")
        );
    }

    #[test]
    fn supports_if_arithmetic_and_function_calls() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                function label(value) { return value + 1; }
                let count = label(1);
                if (count === 2) {
                    document.getElementById("status").textContent = "two";
                }
                "#,
            )
            .expect("script should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("two")
        );
    }

    #[test]
    fn rejects_unknown_identifiers() {
        let document = document("<p>text</p>");
        let mut runtime = ScriptRuntime::new(document);
        let error = runtime
            .execute("missingValue;")
            .expect_err("unknown name should fail");
        assert!(error.message.contains("unknown identifier"));
    }
}
