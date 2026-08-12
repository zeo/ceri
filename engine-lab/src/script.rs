use crate::dom::{Document, Element, Node, NodePath};
use crate::navigation::{APP_ORIGIN, AppUrl, ResourceBundle};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::rc::Rc;
use std::rc::Weak;

type EnvironmentRef = Rc<RefCell<Environment>>;
type ElementHandle = Rc<RefCell<ElementLocation>>;
type ResponseHandle = Rc<ResponseData>;
type PromiseHandle = Rc<RefCell<PromiseData>>;
type ObjectHandle = Rc<RefCell<BTreeMap<String, Value>>>;
type ArrayHandle = Rc<RefCell<Vec<Value>>>;
type LocationHandle = Rc<RefCell<AppUrl>>;
type HistoryHandle = Rc<RefCell<HistoryData>>;
type UrlHandle = Rc<RefCell<UrlData>>;
type UrlSearchParamsHandle = Rc<RefCell<UrlSearchParamsData>>;

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
    Object(ObjectHandle),
    Array(ArrayHandle),
    Element(ElementHandle),
    Style(ElementHandle),
    ClassList(ElementHandle),
    Event(EventData),
    Response(ResponseHandle),
    Promise(PromiseHandle),
    Document,
    Window,
    Location(LocationHandle),
    History(HistoryHandle),
    Url(UrlHandle),
    UrlSearchParams(UrlSearchParamsHandle),
    Console,
    Json,
    UrlConstructor,
    UrlSearchParamsConstructor,
    Function(Rc<Function>),
    Builtin(Builtin),
}

#[derive(Clone)]
enum Builtin {
    DocumentGetElementById,
    DocumentQuerySelector,
    DocumentCreateElement,
    ConsoleLog,
    EventPreventDefault(Rc<RefCell<EventState>>),
    LocationAssign,
    LocationReplace,
    WindowAddEventListener,
    HistoryPushState,
    HistoryReplaceState,
    HistoryBack,
    HistoryForward,
    HistoryGo,
    UrlSearchParamsAppend(UrlSearchParamsHandle),
    UrlSearchParamsDelete(UrlSearchParamsHandle),
    UrlSearchParamsGet(UrlSearchParamsHandle),
    UrlSearchParamsGetAll(UrlSearchParamsHandle),
    UrlSearchParamsHas(UrlSearchParamsHandle),
    UrlSearchParamsSet(UrlSearchParamsHandle),
    UrlSearchParamsSort(UrlSearchParamsHandle),
    UrlSearchParamsToString(UrlSearchParamsHandle),
    UrlToString(UrlHandle),
    JsonParse,
    JsonStringify,
    SetTimeout,
    ClearTimeout,
    RequestAnimationFrame,
    CancelAnimationFrame,
    ElementAddEventListener(NodePath),
    ElementClick(NodePath),
    ElementAppendChild(NodePath),
    ElementSetAttribute(NodePath),
    ElementGetAttribute(NodePath),
    ElementQuerySelector(NodePath),
    ElementClassListAdd(ElementHandle),
    ElementClassListRemove(ElementHandle),
    ElementClassListContains(ElementHandle),
    ElementClassListToggle(ElementHandle),
    Fetch,
    PromiseThen(PromiseHandle),
    PromiseCatch(PromiseHandle),
    ResponseText(ResponseHandle),
    ResponseJson(ResponseHandle),
}

struct Function {
    params: Vec<String>,
    body: Vec<Stmt>,
    environment: EnvironmentRef,
}

#[derive(Clone, PartialEq)]
enum ElementLocation {
    Attached(NodePath),
    Detached(Element),
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
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
    },
    Object(Vec<(String, Expr)>),
    Array(Vec<Expr>),
    Call {
        callee: Box<Expr>,
        arguments: Vec<Expr>,
    },
    New {
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
    resources: Option<ResourceBundle>,
    base_url: Option<AppUrl>,
    global: EnvironmentRef,
    location: LocationHandle,
    history: HistoryHandle,
    listeners: HashMap<NodePath, Vec<EventListener>>,
    window_listeners: HashMap<String, Vec<Value>>,
    logs: Vec<String>,
    clock: u64,
    next_task_id: u64,
    tasks: Vec<ScheduledTask>,
    microtasks: VecDeque<Microtask>,
    pending_navigation: Option<NavigationRequest>,
}

#[derive(Clone)]
struct EventListener {
    event: String,
    callback: Value,
}

#[derive(Clone)]
struct EventData {
    event_type: String,
    target: NodePath,
    current_target: NodePath,
    state: Rc<RefCell<EventState>>,
    history_state: Rc<RefCell<Value>>,
}

#[derive(Default)]
struct EventState {
    default_prevented: bool,
}

struct HistoryData {
    state: Value,
    length: usize,
    index: usize,
}

struct UrlData {
    url: AppUrl,
    search_params: Option<UrlSearchParamsHandle>,
}

struct UrlSearchParamsData {
    pairs: Vec<(String, String)>,
    url: Option<Weak<RefCell<UrlData>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NavigationRequest {
    Navigate {
        reference: String,
        replace_history: bool,
    },
    PushState {
        reference: String,
        state: String,
    },
    ReplaceState {
        reference: String,
        state: String,
    },
    Traverse {
        delta: isize,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EventDispatch {
    pub(crate) handled: bool,
    pub(crate) default_prevented: bool,
}

struct ResponseData {
    url: String,
    status: u16,
    mime_type: String,
    body: Vec<u8>,
}

#[derive(Clone)]
enum PromiseOutcome {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}

struct PromiseData {
    outcome: PromiseOutcome,
    reactions: Vec<PromiseReaction>,
}

struct Microtask {
    callback: Value,
    argument: Value,
    next: PromiseHandle,
}

struct PromiseReaction {
    fulfilled: Option<Value>,
    rejected: Option<Value>,
    next: PromiseHandle,
}

#[derive(Clone, Copy)]
enum TaskKind {
    Timeout,
    AnimationFrame,
}

struct ScheduledTask {
    id: u64,
    due: u64,
    callback: Value,
    kind: TaskKind,
}

const MAX_TASKS_PER_ADVANCE: usize = 10_000;
const MAX_MICROTASKS_PER_DRAIN: usize = 10_000;

impl ScriptRuntime {
    pub fn new(document: Rc<RefCell<Document>>) -> Self {
        Self::with_context(document, None, None)
    }

    pub fn with_resources(
        document: Rc<RefCell<Document>>,
        resources: ResourceBundle,
        base_url: AppUrl,
    ) -> Self {
        Self::with_context(document, Some(resources), Some(base_url))
    }

    fn with_context(
        document: Rc<RefCell<Document>>,
        resources: Option<ResourceBundle>,
        base_url: Option<AppUrl>,
    ) -> Self {
        let global = Rc::new(RefCell::new(Environment::default()));
        let location = Rc::new(RefCell::new(base_url.clone().unwrap_or_else(|| {
            AppUrl::parse("https://app.ceri.invalid/").expect("built-in app origin should parse")
        })));
        let history = Rc::new(RefCell::new(HistoryData {
            state: Value::Null,
            length: 0,
            index: 0,
        }));
        {
            let mut values = global.borrow_mut();
            values
                .values
                .insert("document".to_string(), Value::Document);
            values.values.insert("window".to_string(), Value::Window);
            values
                .values
                .insert("location".to_string(), Value::Location(location.clone()));
            values.values.insert(
                "history".to_string(),
                Value::History(history.clone()),
            );
            values.values.insert("console".to_string(), Value::Console);
            values.values.insert("JSON".to_string(), Value::Json);
            values
                .values
                .insert("URL".to_string(), Value::UrlConstructor);
            values.values.insert(
                "URLSearchParams".to_string(),
                Value::UrlSearchParamsConstructor,
            );
            values
                .values
                .insert("fetch".to_string(), Value::Builtin(Builtin::Fetch));
            values.values.insert(
                "setTimeout".to_string(),
                Value::Builtin(Builtin::SetTimeout),
            );
            values.values.insert(
                "clearTimeout".to_string(),
                Value::Builtin(Builtin::ClearTimeout),
            );
            values.values.insert(
                "requestAnimationFrame".to_string(),
                Value::Builtin(Builtin::RequestAnimationFrame),
            );
            values.values.insert(
                "cancelAnimationFrame".to_string(),
                Value::Builtin(Builtin::CancelAnimationFrame),
            );
        }
        Self {
            document,
            resources,
            base_url,
            global,
            location,
            history,
            listeners: HashMap::new(),
            window_listeners: HashMap::new(),
            logs: Vec::new(),
            clock: 0,
            next_task_id: 1,
            tasks: Vec::new(),
            microtasks: VecDeque::new(),
            pending_navigation: None,
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
            Completion::Normal | Completion::Return(_) => self.drain_microtasks(),
        }
    }

    pub fn click_id(&mut self, id: &str) -> Result<bool, ScriptError> {
        let Some(path) = self.document.borrow().find_element_by_id(id) else {
            return Ok(false);
        };
        self.click_path(path)
    }

    pub fn click_path(&mut self, path: NodePath) -> Result<bool, ScriptError> {
        let dispatch = self.dispatch_event(path.clone(), "click")?;
        self.drain_microtasks()?;
        if !dispatch.default_prevented && self.pending_navigation.is_none() {
            let reference = self.document.borrow().link_reference(&path);
            if let Some(reference) = reference {
                self.request_location_navigation(reference, false);
            }
        }
        Ok(dispatch.handled)
    }

    pub fn dispatch_path_event(
        &mut self,
        path: NodePath,
        event: &str,
    ) -> Result<bool, ScriptError> {
        let dispatch = self.dispatch_event(path, event)?;
        self.drain_microtasks()?;
        Ok(dispatch.handled)
    }

    pub fn logs(&self) -> &[String] {
        &self.logs
    }

    pub fn document(&self) -> Rc<RefCell<Document>> {
        self.document.clone()
    }

    pub fn now(&self) -> u64 {
        self.clock
    }

    pub fn pending_task_count(&self) -> usize {
        self.tasks.len()
    }

    pub(crate) fn take_navigation_request(&mut self) -> Option<NavigationRequest> {
        self.pending_navigation.take()
    }

    pub(crate) fn set_location(&mut self, url: AppUrl) {
        self.base_url = Some(url.clone());
        *self.location.borrow_mut() = url;
    }

    pub(crate) fn set_history_state_serialized(
        &mut self,
        serialized: &str,
    ) -> Result<(), ScriptError> {
        let state = self.parse_json(serialized)?;
        self.history.borrow_mut().state = state;
        Ok(())
    }

    pub(crate) fn set_history_position(&mut self, length: usize, index: usize) {
        let mut history = self.history.borrow_mut();
        history.length = length;
        history.index = index;
    }

    fn apply_history_state_locally(
        &mut self,
        reference: &str,
        serialized: &str,
    ) -> Result<(), ScriptError> {
        let base = self.location.borrow().clone();
        let url = base
            .resolve(reference)
            .map_err(|error| runtime_error(error.to_string()))?;
        let state = self.parse_json(serialized)?;
        self.base_url = Some(url.clone());
        *self.location.borrow_mut() = url;
        self.history.borrow_mut().state = state;
        Ok(())
    }

    pub(crate) fn dispatch_popstate(&mut self, serialized: &str) -> Result<bool, ScriptError> {
        let state = self.parse_json(serialized)?;
        let callbacks = self
            .window_listeners
            .get("popstate")
            .cloned()
            .unwrap_or_default();
        let event = Value::Event(EventData {
            event_type: "popstate".to_string(),
            target: Vec::new(),
            current_target: Vec::new(),
            state: Rc::new(RefCell::new(EventState::default())),
            history_state: Rc::new(RefCell::new(state)),
        });
        for callback in callbacks {
            self.call(callback, vec![event.clone()])?;
        }
        self.drain_microtasks()?;
        Ok(self
            .window_listeners
            .get("popstate")
            .is_some_and(|listeners| !listeners.is_empty()))
    }

    fn request_location_navigation(&mut self, reference: String, replace_history: bool) {
        self.queue_navigation(NavigationRequest::Navigate {
            reference,
            replace_history,
        });
    }

    fn queue_navigation(&mut self, request: NavigationRequest) {
        if self.pending_navigation.is_none() {
            self.pending_navigation = Some(request);
        }
    }

    pub fn advance_time(&mut self, milliseconds: u64) -> Result<usize, ScriptError> {
        self.clock = self.clock.saturating_add(milliseconds);
        let mut executed = 0;
        while let Some(index) = self.next_due_task() {
            if executed == MAX_TASKS_PER_ADVANCE {
                return Err(runtime_error("task queue exceeded the deterministic limit"));
            }
            let task = self.tasks.remove(index);
            let arguments = match task.kind {
                TaskKind::Timeout => Vec::new(),
                TaskKind::AnimationFrame => vec![Value::Number(self.clock as f64)],
            };
            self.call(task.callback, arguments)?;
            executed += 1;
        }
        self.drain_microtasks()?;
        Ok(executed)
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
                match operator {
                    BinaryOperator::And => {
                        if !left.truthy() {
                            return Ok(left);
                        }
                        return self.evaluate(right, environment);
                    }
                    BinaryOperator::Or => {
                        if left.truthy() {
                            return Ok(left);
                        }
                        return self.evaluate(right, environment);
                    }
                    _ => {}
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
            Expr::Index { object, index } => {
                let object = self.evaluate(object, environment.clone())?;
                let index = self.evaluate(index, environment)?;
                self.index(object, index)
            }
            Expr::Object(properties) => {
                let mut object = BTreeMap::new();
                for (property, expression) in properties {
                    object.insert(
                        property.clone(),
                        self.evaluate(expression, environment.clone())?,
                    );
                }
                Ok(Value::Object(Rc::new(RefCell::new(object))))
            }
            Expr::Array(expressions) => {
                let values = expressions
                    .iter()
                    .map(|expression| self.evaluate(expression, environment.clone()))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Value::Array(Rc::new(RefCell::new(values))))
            }
            Expr::Call { callee, arguments } => {
                let callee = self.evaluate(callee, environment.clone())?;
                let arguments = arguments
                    .iter()
                    .map(|argument| self.evaluate(argument, environment.clone()))
                    .collect::<Result<Vec<_>, _>>()?;
                self.call(callee, arguments)
            }
            Expr::New { callee, arguments } => {
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
                if name == "location" {
                    self.request_location_navigation(value.string_value(), false);
                    return Ok(());
                }
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
                "createElement" => Ok(Value::Builtin(Builtin::DocumentCreateElement)),
                _ => Err(runtime_error(format!(
                    "document has no property {property}"
                ))),
            },
            Value::Window => match property {
                "document" => Ok(Value::Document),
                "location" => self
                    .global
                    .borrow()
                    .get("location")
                    .ok_or_else(|| runtime_error("window location is unavailable")),
                "history" => self
                    .global
                    .borrow()
                    .get("history")
                    .ok_or_else(|| runtime_error("window history is unavailable")),
                "console" => Ok(Value::Console),
                "addEventListener" => Ok(Value::Builtin(Builtin::WindowAddEventListener)),
                "JSON" => Ok(Value::Json),
                "fetch" => Ok(Value::Builtin(Builtin::Fetch)),
                "setTimeout" => Ok(Value::Builtin(Builtin::SetTimeout)),
                "clearTimeout" => Ok(Value::Builtin(Builtin::ClearTimeout)),
                "requestAnimationFrame" => Ok(Value::Builtin(Builtin::RequestAnimationFrame)),
                "cancelAnimationFrame" => Ok(Value::Builtin(Builtin::CancelAnimationFrame)),
                _ => Err(runtime_error(format!("window has no property {property}"))),
            },
            Value::Console if property == "log" => Ok(Value::Builtin(Builtin::ConsoleLog)),
            Value::Element(handle) => {
                let path = attached_path(&handle);
                let value = match property {
                    "textContent" => self
                        .element_text_content(&handle)
                        .map(Value::String)
                        .unwrap_or(Value::Undefined),
                    "className" => self
                        .element_attribute(&handle, "class")
                        .map(Value::String)
                        .unwrap_or_else(|| Value::String(String::new())),
                    "id" => self
                        .element_attribute(&handle, "id")
                        .map(Value::String)
                        .unwrap_or_else(|| Value::String(String::new())),
                    "innerHTML" => self
                        .element_inner_html(&handle)
                        .map(Value::String)
                        .unwrap_or_else(|| Value::String(String::new())),
                    "style" => Value::Style(handle),
                    "classList" => Value::ClassList(handle),
                    "querySelector" => {
                        Value::Builtin(Builtin::ElementQuerySelector(path.ok_or_else(|| {
                            runtime_error("detached element cannot query descendants")
                        })?))
                    }
                    "addEventListener" => {
                        Value::Builtin(Builtin::ElementAddEventListener(path.ok_or_else(|| {
                            runtime_error("detached element cannot receive events")
                        })?))
                    }
                    "click" => Value::Builtin(Builtin::ElementClick(
                        path.ok_or_else(|| runtime_error("detached element cannot be clicked"))?,
                    )),
                    "appendChild" => {
                        Value::Builtin(Builtin::ElementAppendChild(path.ok_or_else(|| {
                            runtime_error("detached element cannot receive children")
                        })?))
                    }
                    "setAttribute" => {
                        Value::Builtin(Builtin::ElementSetAttribute(path.ok_or_else(|| {
                            runtime_error("detached element has no document attributes")
                        })?))
                    }
                    "getAttribute" => {
                        Value::Builtin(Builtin::ElementGetAttribute(path.ok_or_else(|| {
                            runtime_error("detached element has no document attributes")
                        })?))
                    }
                    _ => return Err(runtime_error(format!("element has no property {property}"))),
                };
                Ok(value)
            }
            Value::Style(handle) => Ok(self
                .element_style_property(&handle, property)
                .map(Value::String)
                .unwrap_or_else(|| Value::String(String::new()))),
            Value::ClassList(handle) => match property {
                "add" => Ok(Value::Builtin(Builtin::ElementClassListAdd(handle))),
                "remove" => Ok(Value::Builtin(Builtin::ElementClassListRemove(handle))),
                "contains" => Ok(Value::Builtin(Builtin::ElementClassListContains(handle))),
                "toggle" => Ok(Value::Builtin(Builtin::ElementClassListToggle(handle))),
                _ => Err(runtime_error(format!(
                    "classList has no property {property}"
                ))),
            },
            Value::Event(event) => match property {
                "type" => Ok(Value::String(event.event_type.clone())),
                "target" => Ok(Value::Element(attached_handle(event.target.clone()))),
                "currentTarget" => Ok(Value::Element(attached_handle(
                    event.current_target.clone(),
                ))),
                "defaultPrevented" => Ok(Value::Bool(event.state.borrow().default_prevented)),
                "state" => Ok(event.history_state.borrow().clone()),
                "preventDefault" => Ok(Value::Builtin(Builtin::EventPreventDefault(
                    event.state.clone(),
                ))),
                _ => Err(runtime_error(format!("event has no property {property}"))),
            },
            Value::Location(url) => {
                let url = url.borrow();
                match property {
                "href" => Ok(Value::String(url.to_string())),
                "origin" => Ok(Value::String(APP_ORIGIN.to_string())),
                "protocol" => Ok(Value::String("https:".to_string())),
                "host" => Ok(Value::String("app.ceri.invalid".to_string())),
                "hostname" => Ok(Value::String("app.ceri.invalid".to_string())),
                "pathname" => Ok(Value::String(url.path().to_string())),
                "search" => Ok(Value::String(if url.query().is_empty() {
                    String::new()
                } else {
                    format!("?{}", url.query())
                })),
                "hash" => Ok(Value::String(if url.fragment().is_empty() {
                    String::new()
                } else {
                    format!("#{}", url.fragment())
                })),
                "assign" => Ok(Value::Builtin(Builtin::LocationAssign)),
                "replace" => Ok(Value::Builtin(Builtin::LocationReplace)),
                _ => Err(runtime_error(format!("location has no property {property}"))),
                }
            }
            Value::History(history) => match property {
                "state" => Ok(history.borrow().state.clone()),
                "length" => Ok(Value::Number(history.borrow().length as f64)),
                "pushState" => Ok(Value::Builtin(Builtin::HistoryPushState)),
                "replaceState" => Ok(Value::Builtin(Builtin::HistoryReplaceState)),
                "back" => Ok(Value::Builtin(Builtin::HistoryBack)),
                "forward" => Ok(Value::Builtin(Builtin::HistoryForward)),
                "go" => Ok(Value::Builtin(Builtin::HistoryGo)),
                _ => Err(runtime_error(format!("history has no property {property}"))),
            },
            Value::Url(url) => match property {
                "href" => Ok(Value::String(url.borrow().url.to_string())),
                "origin" => Ok(Value::String(APP_ORIGIN.to_string())),
                "protocol" => Ok(Value::String("https:".to_string())),
                "host" | "hostname" => Ok(Value::String("app.ceri.invalid".to_string())),
                "pathname" => Ok(Value::String(url.borrow().url.path().to_string())),
                "search" => Ok(Value::String(search_component(&url.borrow().url))),
                "hash" => Ok(Value::String(fragment_component(&url.borrow().url))),
                "searchParams" => Ok(Value::UrlSearchParams(self.url_search_params(&url)?)),
                "toString" => Ok(Value::Builtin(Builtin::UrlToString(url))),
                _ => Err(runtime_error(format!("URL has no property {property}"))),
            },
            Value::UrlSearchParams(params) => match property {
                "size" => Ok(Value::Number(params.borrow().pairs.len() as f64)),
                "append" => Ok(Value::Builtin(Builtin::UrlSearchParamsAppend(params))),
                "delete" => Ok(Value::Builtin(Builtin::UrlSearchParamsDelete(params))),
                "get" => Ok(Value::Builtin(Builtin::UrlSearchParamsGet(params))),
                "getAll" => Ok(Value::Builtin(Builtin::UrlSearchParamsGetAll(params))),
                "has" => Ok(Value::Builtin(Builtin::UrlSearchParamsHas(params))),
                "set" => Ok(Value::Builtin(Builtin::UrlSearchParamsSet(params))),
                "sort" => Ok(Value::Builtin(Builtin::UrlSearchParamsSort(params))),
                "toString" => Ok(Value::Builtin(Builtin::UrlSearchParamsToString(params))),
                _ => Err(runtime_error(format!(
                    "URLSearchParams has no property {property}"
                ))),
            },
            Value::Response(response) => match property {
                "url" => Ok(Value::String(response.url.clone())),
                "status" => Ok(Value::Number(response.status as f64)),
                "ok" => Ok(Value::Bool((200..300).contains(&response.status))),
                "mimeType" => Ok(Value::String(response.mime_type.clone())),
                "text" => Ok(Value::Builtin(Builtin::ResponseText(response))),
                "json" => Ok(Value::Builtin(Builtin::ResponseJson(response))),
                _ => Err(runtime_error(format!(
                    "response has no property {property}"
                ))),
            },
            Value::Object(object) => Ok(object
                .borrow()
                .get(property)
                .cloned()
                .unwrap_or(Value::Undefined)),
            Value::Array(array) => {
                if property == "length" {
                    Ok(Value::Number(array.borrow().len() as f64))
                } else {
                    let index = property
                        .parse::<usize>()
                        .map_err(|_| runtime_error(format!("array has no property {property}")))?;
                    Ok(array
                        .borrow()
                        .get(index)
                        .cloned()
                        .unwrap_or(Value::Undefined))
                }
            }
            Value::Json => match property {
                "parse" => Ok(Value::Builtin(Builtin::JsonParse)),
                "stringify" => Ok(Value::Builtin(Builtin::JsonStringify)),
                _ => Err(runtime_error(format!("JSON has no property {property}"))),
            },
            Value::Promise(promise) => match property {
                "then" => Ok(Value::Builtin(Builtin::PromiseThen(promise))),
                "catch" => Ok(Value::Builtin(Builtin::PromiseCatch(promise))),
                _ => Err(runtime_error(format!("promise has no property {property}"))),
            },
            _ => Err(runtime_error(format!("value has no property {property}"))),
        }
    }

    fn index(&self, object: Value, index: Value) -> Result<Value, ScriptError> {
        match object {
            Value::Object(object) => Ok(object
                .borrow()
                .get(&index.string_value())
                .cloned()
                .unwrap_or(Value::Undefined)),
            Value::Array(array) => {
                let index = index.number()?;
                if !index.is_finite() || index < 0.0 || index.fract() != 0.0 {
                    return Ok(Value::Undefined);
                }
                Ok(array
                    .borrow()
                    .get(index as usize)
                    .cloned()
                    .unwrap_or(Value::Undefined))
            }
            Value::String(string) => {
                let index = index.number()?;
                if !index.is_finite() || index < 0.0 || index.fract() != 0.0 {
                    return Ok(Value::Undefined);
                }
                Ok(string
                    .chars()
                    .nth(index as usize)
                    .map(|character| Value::String(character.to_string()))
                    .unwrap_or(Value::Undefined))
            }
            _ => Err(runtime_error("value cannot be indexed")),
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
            Value::Element(handle) => match property {
                "textContent" => self.set_element_text_content(&handle, value),
                "className" => self.set_element_attribute(&handle, "class", value),
                "id" => self.set_element_attribute(&handle, "id", value),
                "innerHTML" => self.set_element_inner_html(&handle, value)?,
                _ => false,
            },
            Value::Style(handle) => self.set_element_style_property(&handle, property, value),
            Value::Window if property == "location" => {
                self.request_location_navigation(value, false);
                true
            }
            Value::Location(_) if property == "href" => {
                self.request_location_navigation(value, false);
                true
            }
            Value::Url(url) => self.assign_url_property(&url, property, &value)?,
            _ => false,
        };
        updated
            .then_some(())
            .ok_or_else(|| runtime_error(format!("property {property} is not writable")))
    }

    fn element_text_content(&self, handle: &ElementHandle) -> Option<String> {
        match handle.borrow().clone() {
            ElementLocation::Attached(path) => self.document.borrow().element_text_content(&path),
            ElementLocation::Detached(element) => {
                let mut text = String::new();
                collect_element_text(&element, &mut text);
                Some(text)
            }
        }
    }

    fn element_attribute(&self, handle: &ElementHandle, name: &str) -> Option<String> {
        match handle.borrow().clone() {
            ElementLocation::Attached(path) => {
                self.document.borrow().element_attribute(&path, name)
            }
            ElementLocation::Detached(element) => element.attr(name).map(str::to_string),
        }
    }

    fn element_inner_html(&self, handle: &ElementHandle) -> Option<String> {
        match handle.borrow().clone() {
            ElementLocation::Attached(path) => self.document.borrow().element_inner_html(&path),
            ElementLocation::Detached(element) => Some(element.inner_html()),
        }
    }

    fn element_style_property(&self, handle: &ElementHandle, property: &str) -> Option<String> {
        match handle.borrow().clone() {
            ElementLocation::Attached(path) => {
                self.document.borrow().style_property(&path, property)
            }
            ElementLocation::Detached(element) => element.style_property(property),
        }
    }

    fn set_element_text_content(&mut self, handle: &ElementHandle, text: String) -> bool {
        let location = handle.borrow().clone();
        match location {
            ElementLocation::Attached(path) => self
                .document
                .borrow_mut()
                .set_element_text_content(&path, text),
            ElementLocation::Detached(_) => {
                let mut location = handle.borrow_mut();
                let ElementLocation::Detached(element) = &mut *location else {
                    return false;
                };
                element.children = vec![Node::Text(text)];
                true
            }
        }
    }

    fn set_element_attribute(&mut self, handle: &ElementHandle, name: &str, value: String) -> bool {
        let location = handle.borrow().clone();
        match location {
            ElementLocation::Attached(path) => self
                .document
                .borrow_mut()
                .set_element_attribute(&path, name, value),
            ElementLocation::Detached(_) => {
                let mut location = handle.borrow_mut();
                let ElementLocation::Detached(element) = &mut *location else {
                    return false;
                };
                element.attrs.insert(name.to_ascii_lowercase(), value);
                true
            }
        }
    }

    fn set_element_inner_html(
        &mut self,
        handle: &ElementHandle,
        source: String,
    ) -> Result<bool, ScriptError> {
        let location = handle.borrow().clone();
        match location {
            ElementLocation::Attached(path) => self
                .document
                .borrow_mut()
                .set_element_inner_html(&path, &source)
                .map_err(|error| runtime_error(error.to_string())),
            ElementLocation::Detached(_) => {
                let mut location = handle.borrow_mut();
                let ElementLocation::Detached(element) = &mut *location else {
                    return Err(runtime_error(
                        "element handle changed during innerHTML update",
                    ));
                };
                element
                    .set_inner_html(&source)
                    .map(|()| true)
                    .map_err(|error| runtime_error(error.to_string()))
            }
        }
    }

    fn set_element_style_property(
        &mut self,
        handle: &ElementHandle,
        property: &str,
        value: String,
    ) -> bool {
        let location = handle.borrow().clone();
        match location {
            ElementLocation::Attached(path) => self
                .document
                .borrow_mut()
                .set_style_property(&path, property, value),
            ElementLocation::Detached(_) => {
                let mut location = handle.borrow_mut();
                let ElementLocation::Detached(element) = &mut *location else {
                    return false;
                };
                element.set_style_property(property, value)
            }
        }
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
            Value::UrlConstructor => self.construct_url(arguments),
            Value::UrlSearchParamsConstructor => self.construct_url_search_params(arguments),
            _ => Err(runtime_error("value is not callable")),
        }
    }

    fn construct_url(&self, arguments: Vec<Value>) -> Result<Value, ScriptError> {
        let reference = arguments
            .first()
            .map(Value::string_value)
            .ok_or_else(|| runtime_error("URL constructor requires a URL"))?;
        let base = match arguments.get(1) {
            Some(Value::Url(url)) => url.borrow().url.clone(),
            Some(value) => AppUrl::parse(&value.string_value())
                .map_err(|error| runtime_error(error.to_string()))?,
            None => self.base_url.clone().unwrap_or_else(|| {
                AppUrl::parse("https://app.ceri.invalid/")
                    .expect("built-in app origin should parse")
            }),
        };
        let url = base
            .resolve(&reference)
            .map_err(|error| runtime_error(error.to_string()))?;
        Ok(Value::Url(Rc::new(RefCell::new(UrlData {
            url,
            search_params: None,
        }))))
    }

    fn construct_url_search_params(
        &self,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let value = arguments.first().cloned().unwrap_or(Value::Null);
        let pairs = params_from_value(&value)?;
        Ok(Value::UrlSearchParams(Rc::new(RefCell::new(
            UrlSearchParamsData { pairs, url: None },
        ))))
    }

    fn url_search_params(
        &self,
        url: &UrlHandle,
    ) -> Result<UrlSearchParamsHandle, ScriptError> {
        if let Some(params) = url.borrow().search_params.clone() {
            return Ok(params);
        }
        let pairs = parse_form_urlencoded(url.borrow().url.query())?;
        let params = Rc::new(RefCell::new(UrlSearchParamsData {
            pairs,
            url: Some(Rc::downgrade(url)),
        }));
        url.borrow_mut().search_params = Some(params.clone());
        Ok(params)
    }

    fn assign_url_property(
        &mut self,
        url: &UrlHandle,
        property: &str,
        value: &str,
    ) -> Result<bool, ScriptError> {
        let current = url.borrow().url.clone();
        let updated = match property {
            "href" => current
                .resolve(value)
                .map_err(|error| runtime_error(error.to_string()))?,
            "pathname" => current
                .with_path(value)
                .map_err(|error| runtime_error(error.to_string()))?
                .with_query_fragment(current.query().to_string(), current.fragment().to_string()),
            "search" => current.with_query_fragment(
                value.strip_prefix('?').unwrap_or(value).to_string(),
                current.fragment().to_string(),
            ),
            "hash" => current.with_query_fragment(
                current.query().to_string(),
                value.strip_prefix('#').unwrap_or(value).to_string(),
            ),
            _ => return Ok(false),
        };
        let params = url.borrow().search_params.clone();
        url.borrow_mut().url = updated.clone();
        if let Some(params) = params {
            params.borrow_mut().pairs = parse_form_urlencoded(updated.query())?;
        }
        Ok(true)
    }

    fn sync_url_search_params(&self, params: &UrlSearchParamsHandle) {
        let Some(url) = params.borrow().url.as_ref().and_then(Weak::upgrade) else {
            return;
        };
        let query = serialize_form_urlencoded(&params.borrow().pairs);
        let current = url.borrow().url.clone();
        url.borrow_mut().url = current.with_query_fragment(query, current.fragment().to_string());
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
                    .map(attached_handle)
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
                    .map(attached_handle)
                    .map(Value::Element)
                    .unwrap_or(Value::Null))
            }
            Builtin::DocumentCreateElement => {
                let name = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                if name.is_empty() {
                    return Err(runtime_error("element name must not be empty"));
                }
                Ok(Value::Element(Rc::new(RefCell::new(
                    ElementLocation::Detached(Element::new(&name)),
                ))))
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
            Builtin::EventPreventDefault(state) => {
                state.borrow_mut().default_prevented = true;
                Ok(Value::Undefined)
            }
            Builtin::WindowAddEventListener => {
                let event = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let callback = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                if !matches!(callback, Value::Function(_)) {
                    return Err(runtime_error("window event listener must be a function"));
                }
                self.window_listeners
                    .entry(event)
                    .or_default()
                    .push(callback);
                Ok(Value::Undefined)
            }
            Builtin::LocationAssign => {
                let reference = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_else(|| "undefined".to_string());
                self.request_location_navigation(reference, false);
                Ok(Value::Undefined)
            }
            Builtin::LocationReplace => {
                let reference = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_else(|| "undefined".to_string());
                self.request_location_navigation(reference, true);
                Ok(Value::Undefined)
            }
            Builtin::HistoryPushState => {
                let state = arguments.first().cloned().unwrap_or(Value::Null);
                let state = serialize_json(&state)?;
                let reference = arguments
                    .get(2)
                    .map(Value::string_value)
                    .unwrap_or_default();
                self.apply_history_state_locally(&reference, &state)?;
                let index = self.history.borrow().index;
                self.history.borrow_mut().length = index.saturating_add(2);
                self.queue_navigation(NavigationRequest::PushState { reference, state });
                Ok(Value::Undefined)
            }
            Builtin::HistoryReplaceState => {
                let state = arguments.first().cloned().unwrap_or(Value::Null);
                let state = serialize_json(&state)?;
                let reference = arguments
                    .get(2)
                    .map(Value::string_value)
                    .unwrap_or_default();
                self.apply_history_state_locally(&reference, &state)?;
                self.queue_navigation(NavigationRequest::ReplaceState { reference, state });
                Ok(Value::Undefined)
            }
            Builtin::HistoryBack => {
                self.queue_navigation(NavigationRequest::Traverse { delta: -1 });
                Ok(Value::Undefined)
            }
            Builtin::HistoryForward => {
                self.queue_navigation(NavigationRequest::Traverse { delta: 1 });
                Ok(Value::Undefined)
            }
            Builtin::HistoryGo => {
                let delta = arguments
                    .first()
                    .map(Value::number)
                    .transpose()?
                    .unwrap_or(0.0);
                if !delta.is_finite() || delta.fract() != 0.0 {
                    return Err(runtime_error("history.go offset must be an integer"));
                }
                let delta = isize::try_from(delta as i128)
                    .map_err(|_| runtime_error("history.go offset is out of range"))?;
                self.queue_navigation(NavigationRequest::Traverse { delta });
                Ok(Value::Undefined)
            }
            Builtin::UrlToString(url) => Ok(Value::String(url.borrow().url.to_string())),
            Builtin::UrlSearchParamsAppend(params) => {
                let name = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let value = arguments
                    .get(1)
                    .map(Value::string_value)
                    .unwrap_or_else(|| "undefined".to_string());
                params.borrow_mut().pairs.push((name, value));
                self.sync_url_search_params(&params);
                Ok(Value::Undefined)
            }
            Builtin::UrlSearchParamsDelete(params) => {
                let name = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let value = arguments.get(1).map(Value::string_value);
                params.borrow_mut().pairs.retain(|(entry_name, entry_value)| {
                    entry_name != &name || value.as_ref().is_some_and(|value| entry_value != value)
                });
                self.sync_url_search_params(&params);
                Ok(Value::Undefined)
            }
            Builtin::UrlSearchParamsGet(params) => {
                let name = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                Ok(params
                    .borrow()
                    .pairs
                    .iter()
                    .find(|(entry_name, _)| entry_name == &name)
                    .map(|(_, value)| Value::String(value.clone()))
                    .unwrap_or(Value::Null))
            }
            Builtin::UrlSearchParamsGetAll(params) => {
                let name = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                Ok(Value::Array(Rc::new(RefCell::new(
                    params
                        .borrow()
                        .pairs
                        .iter()
                        .filter(|(entry_name, _)| entry_name == &name)
                        .map(|(_, value)| Value::String(value.clone()))
                        .collect(),
                ))))
            }
            Builtin::UrlSearchParamsHas(params) => {
                let name = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let value = arguments.get(1).map(Value::string_value);
                Ok(Value::Bool(params.borrow().pairs.iter().any(
                    |(entry_name, entry_value)| {
                        entry_name == &name
                            && value.as_ref().is_none_or(|value| entry_value == value)
                    },
                )))
            }
            Builtin::UrlSearchParamsSet(params) => {
                let name = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let value = arguments
                    .get(1)
                    .map(Value::string_value)
                    .unwrap_or_else(|| "undefined".to_string());
                let mut pairs = params.borrow_mut();
                if let Some(index) = pairs
                    .pairs
                    .iter()
                    .position(|(entry_name, _)| entry_name == &name)
                {
                    pairs.pairs[index].1 = value.clone();
                    let mut kept = false;
                    pairs.pairs.retain(|(entry_name, _)| {
                        if entry_name != &name {
                            true
                        } else if kept {
                            false
                        } else {
                            kept = true;
                            true
                        }
                    });
                } else {
                    pairs.pairs.push((name, value));
                }
                drop(pairs);
                self.sync_url_search_params(&params);
                Ok(Value::Undefined)
            }
            Builtin::UrlSearchParamsSort(params) => {
                params
                    .borrow_mut()
                    .pairs
                    .sort_by(|left, right| left.0.cmp(&right.0));
                self.sync_url_search_params(&params);
                Ok(Value::Undefined)
            }
            Builtin::UrlSearchParamsToString(params) => Ok(Value::String(
                serialize_form_urlencoded(&params.borrow().pairs),
            )),
            Builtin::JsonParse => {
                let source = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                self.parse_json(&source)
            }
            Builtin::JsonStringify => {
                let value = arguments.first().cloned().unwrap_or(Value::Undefined);
                serialize_json(&value).map(Value::String)
            }
            Builtin::Fetch => Ok(Value::Promise(self.fetch(arguments.first()))),
            Builtin::PromiseThen(promise) => {
                self.promise_then(promise, arguments.first(), arguments.get(1))
            }
            Builtin::PromiseCatch(promise) => self.promise_then(promise, None, arguments.first()),
            Builtin::ResponseText(response) => Ok(Value::Promise(self.fulfilled_promise(
                Value::String(String::from_utf8_lossy(&response.body).into_owned()),
            ))),
            Builtin::ResponseJson(response) => {
                let source = String::from_utf8_lossy(&response.body);
                match self.parse_json(&source) {
                    Ok(value) => Ok(Value::Promise(self.fulfilled_promise(value))),
                    Err(error) => Ok(Value::Promise(self.rejected_promise(error.message))),
                }
            }
            Builtin::SetTimeout => self.schedule_task(arguments, TaskKind::Timeout),
            Builtin::ClearTimeout => {
                let id = arguments
                    .first()
                    .map(Value::number)
                    .transpose()?
                    .unwrap_or(0.0) as u64;
                self.tasks.retain(|task| task.id != id);
                Ok(Value::Undefined)
            }
            Builtin::RequestAnimationFrame => {
                self.schedule_task(arguments, TaskKind::AnimationFrame)
            }
            Builtin::CancelAnimationFrame => {
                let id = arguments
                    .first()
                    .map(Value::number)
                    .transpose()?
                    .unwrap_or(0.0) as u64;
                self.tasks.retain(|task| task.id != id);
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
                self.click_path(path)?;
                Ok(Value::Undefined)
            }
            Builtin::ElementAppendChild(path) => {
                let child = match arguments.first() {
                    Some(Value::Element(handle)) => handle.clone(),
                    _ => return Err(runtime_error("appendChild expects an element")),
                };
                let location = child.borrow().clone();
                let element = match location {
                    ElementLocation::Detached(element) => element,
                    ElementLocation::Attached(_) => {
                        return Err(runtime_error(
                            "appendChild currently accepts detached elements only",
                        ));
                    }
                };
                let child_path = self
                    .document
                    .borrow_mut()
                    .append_element(&path, element)
                    .ok_or_else(|| {
                        runtime_error("appendChild parent is no longer in the document")
                    })?;
                *child.borrow_mut() = ElementLocation::Attached(child_path);
                Ok(Value::Element(child))
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
            Builtin::ElementQuerySelector(path) => {
                let selector = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                Ok(self
                    .document
                    .borrow()
                    .find_element_by_selector_within(&path, &selector)
                    .map(attached_handle)
                    .map(Value::Element)
                    .unwrap_or(Value::Null))
            }
            Builtin::ElementClassListAdd(handle) => {
                for argument in arguments {
                    let class = class_name(&argument)?;
                    self.add_class(&handle, &class)?;
                }
                Ok(Value::Undefined)
            }
            Builtin::ElementClassListRemove(handle) => {
                for argument in arguments {
                    let class = class_name(&argument)?;
                    self.remove_class(&handle, &class)?;
                }
                Ok(Value::Undefined)
            }
            Builtin::ElementClassListContains(handle) => {
                let class = first_class_name(&arguments)?;
                Ok(Value::Bool(self.has_class(&handle, &class)?))
            }
            Builtin::ElementClassListToggle(handle) => {
                let class = first_class_name(&arguments)?;
                let force = arguments.get(1).map(Value::truthy);
                Ok(Value::Bool(self.toggle_class(&handle, &class, force)?))
            }
        }
    }

    fn dispatch_event(&mut self, path: NodePath, event: &str) -> Result<EventDispatch, ScriptError> {
        let target = path.clone();
        let mut current = path;
        let mut had_callbacks = false;
        let state = Rc::new(RefCell::new(EventState::default()));
        loop {
            let callbacks = self
                .listeners
                .get(&current)
                .into_iter()
                .flatten()
                .filter(|listener| listener.event == event)
                .map(|listener| listener.callback.clone())
                .collect::<Vec<_>>();
            had_callbacks |= !callbacks.is_empty();
            let event_value = Value::Event(EventData {
                event_type: event.to_string(),
                target: target.clone(),
                current_target: current.clone(),
                state: state.clone(),
                history_state: Rc::new(RefCell::new(Value::Null)),
            });
            for callback in callbacks {
                self.call(callback, vec![event_value.clone()])?;
            }
            if current.is_empty() {
                break;
            }
            current.pop();
        }
        Ok(EventDispatch {
            handled: had_callbacks,
            default_prevented: state.borrow().default_prevented,
        })
    }

    fn fulfilled_promise(&self, value: Value) -> PromiseHandle {
        Rc::new(RefCell::new(PromiseData {
            outcome: PromiseOutcome::Fulfilled(value),
            reactions: Vec::new(),
        }))
    }

    fn rejected_promise(&self, reason: impl Into<String>) -> PromiseHandle {
        Rc::new(RefCell::new(PromiseData {
            outcome: PromiseOutcome::Rejected(Value::String(reason.into())),
            reactions: Vec::new(),
        }))
    }

    fn promise_then(
        &mut self,
        promise: PromiseHandle,
        fulfilled: Option<&Value>,
        rejected: Option<&Value>,
    ) -> Result<Value, ScriptError> {
        for callback in fulfilled.iter().chain(rejected.iter()) {
            if !matches!(callback, Value::Function(_)) {
                return Err(runtime_error("promise callback must be a function"));
            }
        }
        let next = Rc::new(RefCell::new(PromiseData {
            outcome: PromiseOutcome::Pending,
            reactions: Vec::new(),
        }));
        let reaction = PromiseReaction {
            fulfilled: fulfilled.cloned(),
            rejected: rejected.cloned(),
            next: next.clone(),
        };
        let outcome = {
            let mut promise = promise.borrow_mut();
            match &promise.outcome {
                PromiseOutcome::Pending => {
                    promise.reactions.push(reaction);
                    return Ok(Value::Promise(next));
                }
                PromiseOutcome::Fulfilled(value) => PromiseOutcome::Fulfilled(value.clone()),
                PromiseOutcome::Rejected(reason) => PromiseOutcome::Rejected(reason.clone()),
            }
        };
        self.queue_reaction(reaction, outcome);
        Ok(Value::Promise(next))
    }

    fn queue_reaction(&mut self, reaction: PromiseReaction, outcome: PromiseOutcome) {
        let (callback, argument) = match outcome.clone() {
            PromiseOutcome::Pending => return,
            PromiseOutcome::Fulfilled(value) => (reaction.fulfilled, value),
            PromiseOutcome::Rejected(reason) => (reaction.rejected, reason),
        };
        if let Some(callback) = callback {
            self.microtasks.push_back(Microtask {
                callback,
                argument,
                next: reaction.next,
            });
        } else {
            self.settle_promise(&reaction.next, outcome);
        }
    }

    fn settle_promise(&mut self, promise: &PromiseHandle, outcome: PromiseOutcome) {
        let reactions = {
            let mut promise = promise.borrow_mut();
            if !matches!(promise.outcome, PromiseOutcome::Pending) {
                return;
            }
            promise.outcome = outcome.clone();
            std::mem::take(&mut promise.reactions)
        };
        for reaction in reactions {
            self.queue_reaction(reaction, outcome.clone());
        }
    }

    fn follow_promise(&mut self, next: PromiseHandle, promise: PromiseHandle) {
        let reaction = PromiseReaction {
            fulfilled: None,
            rejected: None,
            next: next.clone(),
        };
        let outcome = {
            let mut promise = promise.borrow_mut();
            match &promise.outcome {
                PromiseOutcome::Pending => {
                    promise.reactions.push(reaction);
                    return;
                }
                PromiseOutcome::Fulfilled(value) => PromiseOutcome::Fulfilled(value.clone()),
                PromiseOutcome::Rejected(reason) => PromiseOutcome::Rejected(reason.clone()),
            }
        };
        self.queue_reaction(reaction, outcome);
    }

    fn drain_microtasks(&mut self) -> Result<(), ScriptError> {
        let mut executed = 0;
        while let Some(task) = self.microtasks.pop_front() {
            if executed == MAX_MICROTASKS_PER_DRAIN {
                return Err(runtime_error(
                    "microtask queue exceeded the deterministic limit",
                ));
            }
            let outcome = self.call(task.callback, vec![task.argument]);
            match outcome {
                Ok(Value::Promise(promise)) => self.follow_promise(task.next, promise),
                Ok(value) => {
                    self.settle_promise(&task.next, PromiseOutcome::Fulfilled(value));
                }
                Err(error) => {
                    self.settle_promise(
                        &task.next,
                        PromiseOutcome::Rejected(Value::String(error.to_string())),
                    );
                }
            }
            executed += 1;
        }
        Ok(())
    }

    fn fetch(&self, reference: Option<&Value>) -> PromiseHandle {
        let reference = reference
            .map(Value::string_value)
            .unwrap_or_else(|| "undefined".to_string());
        let Some(resources) = &self.resources else {
            return self.rejected_promise("fetch is unavailable without a resource bundle");
        };
        let Some(base_url) = &self.base_url else {
            return self.rejected_promise("fetch is unavailable without a document URL");
        };
        let Ok(url) = base_url.resolve(&reference) else {
            return self.rejected_promise(format!("fetch rejected URL {reference}"));
        };
        let display_url = url.to_string();
        let Some(resource) = resources.resource(url.path()) else {
            return self.fulfilled_promise(Value::Response(Rc::new(ResponseData {
                url: display_url,
                status: 404,
                mime_type: String::new(),
                body: Vec::new(),
            })));
        };
        self.fulfilled_promise(Value::Response(Rc::new(ResponseData {
            url: display_url,
            status: 200,
            mime_type: resource.mime_type().to_string(),
            body: resource.body().to_vec(),
        })))
    }

    fn parse_json(&mut self, source: &str) -> Result<Value, ScriptError> {
        let mut parser = Parser::new(source)?;
        let expression = parser.parse_expression()?;
        if !parser.is(TokenKind::Eof) {
            return Err(runtime_error("JSON input has trailing tokens"));
        }
        self.evaluate(&expression, self.global.clone())
    }

    fn schedule_task(
        &mut self,
        arguments: Vec<Value>,
        kind: TaskKind,
    ) -> Result<Value, ScriptError> {
        let callback = arguments.first().cloned().unwrap_or(Value::Undefined);
        if !matches!(callback, Value::Function(_)) {
            return Err(runtime_error("scheduled task must be a function"));
        }
        let delay = match kind {
            TaskKind::Timeout => arguments
                .get(1)
                .map(Value::number)
                .transpose()?
                .map_or(0, delay_milliseconds),
            TaskKind::AnimationFrame => 16,
        };
        let id = self.next_task_id;
        self.next_task_id = self.next_task_id.saturating_add(1);
        self.tasks.push(ScheduledTask {
            id,
            due: self.clock.saturating_add(delay),
            callback,
            kind,
        });
        Ok(Value::Number(id as f64))
    }

    fn next_due_task(&self) -> Option<usize> {
        self.tasks
            .iter()
            .enumerate()
            .filter(|(_, task)| task.due <= self.clock)
            .min_by_key(|(_, task)| (task.due, task.id))
            .map(|(index, _)| index)
    }

    fn has_class(&self, handle: &ElementHandle, class: &str) -> Result<bool, ScriptError> {
        match handle.borrow().clone() {
            ElementLocation::Attached(path) => {
                Ok(self.document.borrow().element_has_class(&path, class))
            }
            ElementLocation::Detached(element) => Ok(element.has_class(class)),
        }
    }

    fn add_class(&mut self, handle: &ElementHandle, class: &str) -> Result<(), ScriptError> {
        let location = handle.borrow().clone();
        match location {
            ElementLocation::Attached(path) => {
                self.document.borrow_mut().add_element_class(&path, class);
            }
            ElementLocation::Detached(_) => {
                let mut location = handle.borrow_mut();
                let ElementLocation::Detached(element) = &mut *location else {
                    return Err(runtime_error("element handle changed during classList.add"));
                };
                element.add_class(class);
            }
        }
        Ok(())
    }

    fn remove_class(&mut self, handle: &ElementHandle, class: &str) -> Result<(), ScriptError> {
        let location = handle.borrow().clone();
        match location {
            ElementLocation::Attached(path) => {
                self.document
                    .borrow_mut()
                    .remove_element_class(&path, class);
            }
            ElementLocation::Detached(_) => {
                let mut location = handle.borrow_mut();
                let ElementLocation::Detached(element) = &mut *location else {
                    return Err(runtime_error(
                        "element handle changed during classList.remove",
                    ));
                };
                element.remove_class(class);
            }
        }
        Ok(())
    }

    fn toggle_class(
        &mut self,
        handle: &ElementHandle,
        class: &str,
        force: Option<bool>,
    ) -> Result<bool, ScriptError> {
        let location = handle.borrow().clone();
        Ok(match location {
            ElementLocation::Attached(path) => self
                .document
                .borrow_mut()
                .toggle_element_class(&path, class, force)
                .ok_or_else(|| runtime_error("element handle is no longer in the document"))?,
            ElementLocation::Detached(_) => {
                let mut location = handle.borrow_mut();
                let ElementLocation::Detached(element) = &mut *location else {
                    return Err(runtime_error(
                        "element handle changed during classList.toggle",
                    ));
                };
                element.toggle_class(class, force)
            }
        })
    }
}

fn attached_handle(path: NodePath) -> ElementHandle {
    Rc::new(RefCell::new(ElementLocation::Attached(path)))
}

fn attached_path(handle: &ElementHandle) -> Option<NodePath> {
    match &*handle.borrow() {
        ElementLocation::Attached(path) => Some(path.clone()),
        ElementLocation::Detached(_) => None,
    }
}

fn collect_element_text(element: &Element, output: &mut String) {
    for child in &element.children {
        match child {
            Node::Element(child) => collect_element_text(child, output),
            Node::Text(text) => output.push_str(text),
        }
    }
}

fn delay_milliseconds(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        value.floor() as u64
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
            Self::Object(_) => "[object Object]".to_string(),
            Self::Array(_) => "[array]".to_string(),
            Self::Element(_) => "[element]".to_string(),
            Self::Style(_) => "[style]".to_string(),
            Self::ClassList(_) => "[classList]".to_string(),
            Self::Event(_) => "[event]".to_string(),
            Self::Response(_) => "[response]".to_string(),
            Self::Promise(_) => "[promise]".to_string(),
            Self::Document => "[document]".to_string(),
            Self::Window => "[window]".to_string(),
            Self::Location(value) => value.borrow().to_string(),
            Self::History(_) => "[object History]".to_string(),
            Self::Url(value) => value.borrow().url.to_string(),
            Self::UrlSearchParams(value) => serialize_form_urlencoded(&value.borrow().pairs),
            Self::Console => "[console]".to_string(),
            Self::Json => "[object JSON]".to_string(),
            Self::UrlConstructor | Self::UrlSearchParamsConstructor => {
                "[function URL]".to_string()
            }
            Self::Function(_) => "[function]".to_string(),
            Self::Builtin(_) => "[function]".to_string(),
        }
    }

    fn display_string(&self) -> String {
        self.string_value()
    }
}

fn serialize_json(value: &Value) -> Result<String, ScriptError> {
    match value {
        Value::Undefined => Ok("null".to_string()),
        Value::Null => Ok("null".to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) if value.is_finite() => Ok(if value.fract() == 0.0 {
            format!("{value:.0}")
        } else {
            value.to_string()
        }),
        Value::Number(_) => Ok("null".to_string()),
        Value::String(value) => Ok(escape_json_string(value)),
        Value::Array(values) => values
            .borrow()
            .iter()
            .map(serialize_json)
            .collect::<Result<Vec<_>, _>>()
            .map(|values| format!("[{}]", values.join(","))),
        Value::Object(properties) => properties
            .borrow()
            .iter()
            .map(|(key, value)| {
                Ok(format!(
                    "{}:{}",
                    escape_json_string(key),
                    serialize_json(value)?
                ))
            })
            .collect::<Result<Vec<_>, ScriptError>>()
            .map(|properties| format!("{{{}}}", properties.join(","))),
        _ => Err(runtime_error("value cannot be serialized as JSON")),
    }
}

fn escape_json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write;
                let _ = write!(escaped, "\\u{:04x}", character as u32);
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

fn search_component(url: &AppUrl) -> String {
    if url.query().is_empty() {
        String::new()
    } else {
        format!("?{}", url.query())
    }
}

fn fragment_component(url: &AppUrl) -> String {
    if url.fragment().is_empty() {
        String::new()
    } else {
        format!("#{}", url.fragment())
    }
}

fn params_from_value(value: &Value) -> Result<Vec<(String, String)>, ScriptError> {
    match value {
        Value::Undefined | Value::Null => Ok(Vec::new()),
        Value::String(value) => parse_form_urlencoded(value.strip_prefix('?').unwrap_or(value)),
        Value::UrlSearchParams(params) => Ok(params.borrow().pairs.clone()),
        Value::Object(object) => Ok(object
            .borrow()
            .iter()
            .map(|(name, value)| (name.clone(), value.string_value()))
            .collect()),
        Value::Array(array) => {
            let mut pairs = Vec::new();
            for entry in array.borrow().iter() {
                let Value::Array(entry) = entry else {
                    return Err(runtime_error(
                        "URLSearchParams sequence entries must be arrays",
                    ));
                };
                let entry = entry.borrow();
                if entry.len() != 2 {
                    return Err(runtime_error(
                        "URLSearchParams sequence entries need two values",
                    ));
                }
                pairs.push((entry[0].string_value(), entry[1].string_value()));
            }
            Ok(pairs)
        }
        _ => parse_form_urlencoded(&value.string_value()),
    }
}

fn parse_form_urlencoded(query: &str) -> Result<Vec<(String, String)>, ScriptError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    query
        .split('&')
        .map(|entry| {
            let (name, value) = entry.split_once('=').map_or((entry, ""), |pair| pair);
            Ok((decode_form_component(name)?, decode_form_component(value)?))
        })
        .collect()
}

fn decode_form_component(value: &str) -> Result<String, ScriptError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' => {
                let high = bytes
                    .get(index + 1)
                    .and_then(|byte| hex_value(*byte))
                    .ok_or_else(|| runtime_error("URLSearchParams has invalid percent encoding"))?;
                let low = bytes
                    .get(index + 2)
                    .and_then(|byte| hex_value(*byte))
                    .ok_or_else(|| runtime_error("URLSearchParams has invalid percent encoding"))?;
                decoded.push((high << 4) | low);
                index += 2;
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8(decoded)
        .map_err(|_| runtime_error("URLSearchParams contains invalid UTF-8"))
}

fn serialize_form_urlencoded(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(name, value)| {
            format!(
                "{}={}",
                encode_form_component(name),
                encode_form_component(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn encode_form_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                encoded.push(*byte as char)
            }
            b' ' => encoded.push('+'),
            byte => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                encoded.push('%');
                encoded.push(HEX[(byte >> 4) as usize] as char);
                encoded.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    encoded
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn class_name(value: &Value) -> Result<String, ScriptError> {
    let class = value.string_value();
    let trimmed = class.trim();
    if trimmed.is_empty()
        || trimmed
            .chars()
            .any(|character| character.is_ascii_whitespace())
    {
        return Err(runtime_error(
            "classList token must be a single non-empty name",
        ));
    }
    Ok(trimmed.to_string())
}

fn first_class_name(arguments: &[Value]) -> Result<String, ScriptError> {
    arguments
        .first()
        .ok_or_else(|| runtime_error("classList method requires a class name"))
        .and_then(class_name)
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
        (Value::Url(left), Value::Url(right)) => Rc::ptr_eq(left, right),
        (Value::UrlSearchParams(left), Value::UrlSearchParams(right)) => Rc::ptr_eq(left, right),
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
            } else if self.take(TokenKind::LBracket) {
                let index = self.parse_expression()?;
                self.expect(TokenKind::RBracket, "index expression")?;
                expression = Expr::Index {
                    object: Box::new(expression),
                    index: Box::new(index),
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
            TokenKind::New => {
                let callee = self.parse_primary()?;
                let arguments = self.parse_argument_list("constructor")?;
                Ok(Expr::New {
                    callee: Box::new(callee),
                    arguments,
                })
            }
            TokenKind::Identifier(name) => {
                if self.take(TokenKind::Arrow) {
                    Ok(self.parse_arrow_body(vec![name])?)
                } else {
                    Ok(Expr::Identifier(name))
                }
            }
            TokenKind::Function => self.parse_function_expression(),
            TokenKind::LBrace => self.parse_object_literal(),
            TokenKind::LBracket => self.parse_array_literal(),
            TokenKind::LParen => {
                let expression = self.parse_expression()?;
                self.expect(TokenKind::RParen, "parenthesized expression")?;
                Ok(expression)
            }
            other => Err(self.error(format!("unexpected token {other:?}"), token.offset)),
        }
    }

    fn parse_object_literal(&mut self) -> Result<Expr, ScriptError> {
        let mut properties = Vec::new();
        if !self.is(TokenKind::RBrace) {
            loop {
                let key = self.advance();
                let key = match key.kind {
                    TokenKind::Identifier(key) | TokenKind::String(key) => key,
                    _ => {
                        return Err(
                            self.error("expected object property name".to_string(), key.offset)
                        );
                    }
                };
                self.expect(TokenKind::Colon, "object property")?;
                properties.push((key, self.parse_expression()?));
                if !self.take(TokenKind::Comma) {
                    break;
                }
                if self.is(TokenKind::RBrace) {
                    break;
                }
            }
        }
        self.expect(TokenKind::RBrace, "object literal")?;
        Ok(Expr::Object(properties))
    }

    fn parse_array_literal(&mut self) -> Result<Expr, ScriptError> {
        let mut values = Vec::new();
        if !self.is(TokenKind::RBracket) {
            loop {
                values.push(self.parse_expression()?);
                if !self.take(TokenKind::Comma) {
                    break;
                }
                if self.is(TokenKind::RBracket) {
                    break;
                }
            }
        }
        self.expect(TokenKind::RBracket, "array literal")?;
        Ok(Expr::Array(values))
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

    fn parse_argument_list(&mut self, context: &str) -> Result<Vec<Expr>, ScriptError> {
        self.expect(TokenKind::LParen, context)?;
        let mut arguments = Vec::new();
        if !self.is(TokenKind::RParen) {
            loop {
                arguments.push(self.parse_expression()?);
                if !self.take(TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(TokenKind::RParen, context)?;
        Ok(arguments)
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
    New,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Colon,
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
                "new" => TokenKind::New,
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
            [b'[', ..] => (TokenKind::LBracket, 1),
            [b']', ..] => (TokenKind::RBracket, 1),
            [b',', ..] => (TokenKind::Comma, 1),
            [b':', ..] => (TokenKind::Colon, 1),
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
    fn queries_descendants_from_an_element_handle() {
        let document = document(
            "<main><section class='panel'><p class='card'>inside</p></section><p class='card'>outside</p><p id='status'>idle</p></main>",
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const panel = document.querySelector(".panel");
                const card = panel.querySelector(".panel .card");
                document.getElementById("status").textContent = card.textContent;
                "##,
            )
            .expect("subtree query should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("inside")
        );
    }

    #[test]
    fn replaces_and_serializes_inner_html_fragments() {
        let document = document("<main id='mount'></main><p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const mount = document.getElementById("mount");
                const status = document.getElementById("status");
                const card = document.createElement("article");
                card.innerHTML = "<p class='created'>inside &amp; ready</p>";
                mount.appendChild(card);
                const nested = mount.querySelector(".created");
                status.textContent = nested.textContent + "|" + card.innerHTML;
                mount.innerHTML = "<p class='replacement'>new</p>";
                status.textContent = mount.querySelector(".replacement").textContent + "|" + mount.innerHTML;
                "##,
            )
            .expect("innerHTML mutations should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("new|<p class=\"replacement\">new</p>")
        );
        let mount = document
            .borrow()
            .find_element_by_id("mount")
            .expect("mount should exist");
        assert_eq!(
            document.borrow().element_inner_html(&mount).as_deref(),
            Some("<p class=\"replacement\">new</p>")
        );
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
    fn bubbles_events_with_target_and_current_target() {
        let document = document(
            "<main id='root'><button id='button'><span id='label'>before</span></button><p id='status'>idle</p></main>",
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const root = document.getElementById("root");
                const button = document.getElementById("button");
                const label = document.getElementById("label");
                const status = document.getElementById("status");
                button.addEventListener("click", (event) => {
                    status.textContent = event.type + ":" + event.target.id + ":" + event.currentTarget.id;
                });
                root.addEventListener("click", (event) => {
                    status.textContent = status.textContent + "|" + event.currentTarget.id;
                });
                label.click();
                "##,
            )
            .expect("listeners should register and bubble");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("click:label:button|root")
        );
        assert!(runtime.click_id("label").expect("click should dispatch"));
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("click:label:button|root")
        );
    }

    #[test]
    fn event_default_can_be_cancelled() {
        let document = document(
            r#"<a id="go" href="/next.html">open</a><p id="status">idle</p>"#,
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const link = document.getElementById("go");
                const status = document.getElementById("status");
                link.addEventListener("click", (event) => {
                    event.preventDefault();
                    status.textContent = event.defaultPrevented;
                });
                "#,
            )
            .expect("listener should register");
        assert!(runtime.click_id("go").expect("click should dispatch"));
        assert_eq!(
            document
                .borrow()
                .element_text_content(
                    &document
                        .borrow()
                        .find_element_by_id("status")
                        .expect("status should exist"),
                )
                .as_deref(),
            Some("true")
        );
        assert!(runtime.take_navigation_request().is_none());
    }

    #[test]
    fn click_turn_drains_promise_microtasks() {
        let document = document(r#"<button id="go">load</button><p id="status">idle</p>"#);
        let mut resources = ResourceBundle::default();
        resources
            .insert("/data.txt", "text/plain", b"ready".to_vec())
            .expect("data resource should be accepted");
        let mut runtime = ScriptRuntime::with_resources(
            document.clone(),
            resources,
            AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse"),
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                document.getElementById("go").addEventListener("click", () => {
                    fetch("/data.txt").then((response) => response.text()).then((body) => {
                        status.textContent = body;
                    });
                });
                "#,
            )
            .expect("listener should register");
        assert!(runtime.click_id("go").expect("click should dispatch"));
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("ready")
        );
    }

    #[test]
    fn url_and_search_params_support_spa_query_workflows() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::with_resources(
            document.clone(),
            ResourceBundle::default(),
            AppUrl::parse("https://app.ceri.invalid/docs/index.html")
                .expect("base URL should parse"),
        );
        runtime
            .execute(
                r#"
                const url = new URL("/search?q=rust&tag=one&tag=two");
                const params = url.searchParams;
                params.set("q", "browser engine");
                params.append("tag", "three");
                params.delete("tag", "one");
                params.sort();
                document.getElementById("status").textContent = url.pathname + "|" + params.get("q") + "|" + params.getAll("tag").length + "|" + params.toString() + "|" + url.href;
                "#,
            )
            .expect("URL APIs should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some(
                "/search|browser engine|2|q=browser+engine&tag=two&tag=three|https://app.ceri.invalid/search?q=browser+engine&tag=two&tag=three",
            )
        );
    }

    #[test]
    fn url_constructor_resolves_against_explicit_app_base() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const base = new URL("https://app.ceri.invalid/docs/index.html?old=1");
                const next = new URL("../next.html?x=1", base);
                document.getElementById("status").textContent = next.href + "|" + next.origin + "|" + next.search;
                "#,
            )
            .expect("URL constructor should accept a base");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("https://app.ceri.invalid/next.html?x=1|https://app.ceri.invalid|?x=1")
        );

        let error = runtime
            .execute("new URL(\"https://other.invalid/\");")
            .expect_err("cross-origin URL should be rejected");
        assert!(error.message.contains("outside the app origin"));
    }

    #[test]
    fn location_exposes_url_parts_and_queues_replacement() {
        let document = document(r#"<p id="status">idle</p>"#);
        let mut runtime = ScriptRuntime::with_resources(
            document.clone(),
            ResourceBundle::default(),
            AppUrl::parse("https://app.ceri.invalid/docs/index.html?mode=lab#top")
                .expect("location URL should parse"),
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                status.textContent = location.href + "|" + location.pathname + "|" + location.search + "|" + location.hash + "|" + window.location.origin;
                location.replace("../next.html");
                "#,
            )
            .expect("location script should execute");
        assert_eq!(
            document
                .borrow()
                .element_text_content(
                    &document
                        .borrow()
                        .find_element_by_id("status")
                        .expect("status should exist"),
                )
                .as_deref(),
            Some(
                "https://app.ceri.invalid/docs/index.html?mode=lab#top|/docs/index.html|?mode=lab|#top|https://app.ceri.invalid",
            )
        );
        assert_eq!(
            runtime.take_navigation_request(),
            Some(NavigationRequest::Navigate {
                reference: "../next.html".to_string(),
                replace_history: true,
            })
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
    fn executes_timeout_tasks_after_deterministic_time_advance() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                setTimeout(() => { status.textContent = "later"; }, 25);
                "#,
            )
            .expect("timeout should schedule");
        assert_eq!(runtime.pending_task_count(), 1);
        assert_eq!(runtime.advance_time(24).expect("clock should advance"), 0);
        assert_eq!(runtime.advance_time(1).expect("timeout should run"), 1);
        assert_eq!(runtime.now(), 25);
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("later")
        );
    }

    #[test]
    fn animation_frames_receive_the_deterministic_timestamp() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                requestAnimationFrame((timestamp) => { status.textContent = timestamp; });
                "#,
            )
            .expect("animation frame should schedule");
        assert_eq!(runtime.advance_time(15).expect("clock should advance"), 0);
        assert_eq!(runtime.advance_time(1).expect("frame should run"), 1);
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("16")
        );
    }

    #[test]
    fn canceled_tasks_do_not_run() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const task = window.setTimeout(() => { status.textContent = "bad"; }, 0);
                window.clearTimeout(task);
                "#,
            )
            .expect("task should schedule and cancel");
        assert_eq!(runtime.advance_time(0).expect("clock should advance"), 0);
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("idle")
        );
    }

    #[test]
    fn creates_and_appends_a_detached_element_with_a_stable_handle() {
        let document = document("<main id='mount'><p>existing</p></main>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const mount = document.getElementById("mount");
                const card = document.createElement("section");
                card.className = "created";
                card.textContent = "created before append";
                card.style.color = "#42d3b4";
                mount.appendChild(card);
                card.textContent = "created after append";
                "##,
            )
            .expect("DOM mutation should execute");
        let created = document
            .borrow()
            .find_element_by_selector(".created")
            .expect("created element should be attached");
        assert_eq!(
            document.borrow().element_text_content(&created).as_deref(),
            Some("created after append")
        );
        assert_eq!(
            document
                .borrow()
                .style_property(&created, "color")
                .as_deref(),
            Some("#42d3b4")
        );
    }

    #[test]
    fn mutates_attached_and_detached_classes_through_class_list() {
        let document = document("<main id='mount'><p id='status' class='ready'>idle</p></main>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const mount = document.getElementById("mount");
                const status = document.getElementById("status");
                status.classList.add("active", "open");
                status.classList.remove("ready");
                status.classList.toggle("open");
                status.classList.toggle("selected", true);
                if (!status.classList.contains("open") && status.classList.contains("selected")) {
                    status.textContent = status.className;
                }
                const card = document.createElement("section");
                card.classList.add("created");
                mount.appendChild(card);
                card.classList.toggle("created", false);
                "##,
            )
            .expect("classList mutations should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document
                .borrow()
                .element_attribute(&status, "class")
                .as_deref(),
            Some("active selected")
        );
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("active selected")
        );
        let created = document
            .borrow()
            .find_element_by_selector("section")
            .expect("created element should exist");
        assert_eq!(
            document
                .borrow()
                .element_attribute(&created, "class")
                .as_deref(),
            Some("")
        );
    }

    #[test]
    fn fetch_reads_a_same_origin_packaged_resource() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/data.json",
                "application/json",
                br#"{"message":"hello"}"#.to_vec(),
            )
            .expect("resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                fetch("./data.json").then((response) => {
                    response.text().then((body) => {
                        status.textContent = response.status + "|" + response.ok + "|" + response.mimeType + "|" + response.url + "|" + body;
                    });
                });
                "#,
            )
            .expect("packaged promise fetch should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some(
                "200|true|application/json|https://app.ceri.invalid/data.json|{\"message\":\"hello\"}"
            )
        );
    }

    #[test]
    fn fetch_reports_missing_and_cross_origin_resources_without_leaving_the_page() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert("/index.html", "text/html", b"<p>shell</p>".to_vec())
            .expect("shell should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                let missing = "";
                fetch("/missing.json").then((response) => {
                    missing = response.status + "|" + response.ok;
                });
                fetch("https://other.invalid/data.json").catch((error) => {
                    status.textContent = missing + "|0|false";
                });
                "#,
            )
            .expect("fetch failures should remain promise values");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("404|false|0|false")
        );
    }

    #[test]
    fn response_json_returns_structured_values_through_the_promise_chain() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/data.json",
                "application/json",
                br#"{"message":"hello","items":[4,5]}"#.to_vec(),
            )
            .expect("resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                fetch("/data.json")
                    .then((response) => response.json())
                    .then((data) => {
                        status.textContent = data.message + "|" + data.items[0] + "|" + data.items.length;
                    });
                "#,
            )
            .expect("response.json should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("hello|4|2")
        );
    }

    #[test]
    fn response_json_rejects_invalid_payloads() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert("/broken.json", "application/json", b"{broken".to_vec())
            .expect("resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                fetch("/broken.json")
                    .then((response) => response.json())
                    .catch((error) => { status.textContent = "invalid"; });
                "#,
            )
            .expect("invalid JSON should reject the response promise");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("invalid")
        );
    }

    #[test]
    fn chained_promise_rejection_reaches_catch() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime =
            ScriptRuntime::with_resources(document.clone(), ResourceBundle::default(), base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                fetch("https://other.invalid/data.json")
                    .then((response) => { status.textContent = "unexpected"; })
                    .catch((error) => { status.textContent = "caught"; });
                "#,
            )
            .expect("chained rejection should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("caught")
        );
    }

    #[test]
    fn promise_callbacks_run_after_the_current_script_turn() {
        let document = document("<p id='status'>start</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert("/data.txt", "text/plain", b"ready".to_vec())
            .expect("resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                fetch("/data.txt").then(() => {
                    status.textContent = status.textContent + "|callback";
                });
                status.textContent = status.textContent + "|sync";
                "#,
            )
            .expect("promise callback should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("start|sync|callback")
        );
    }

    #[test]
    fn json_parse_and_stringify_support_objects_arrays_and_indexes() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const parsed = JSON.parse("{\"message\":\"ready\",\"items\":[1,2]}");
                const serialized = JSON.stringify({ ok: true, count: parsed.items.length });
                status.textContent = parsed.message + "|" + parsed.items[1] + "|" + serialized;
                "#,
            )
            .expect("JSON operations should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("ready|2|{\"count\":2,\"ok\":true}")
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
