use crate::dom::{Document, Element, Node, NodePath};
use crate::css::StyleState;
use crate::navigation::{is_javascript, is_json, APP_ORIGIN, AppUrl, ResourceBundle};
use crate::paint::CanvasCommand;
use crate::Color;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::rc::Rc;
use std::rc::Weak;

type EnvironmentRef = Rc<RefCell<Environment>>;
type ElementHandle = Rc<RefCell<ElementLocation>>;
type ResponseHandle = Rc<ResponseData>;
type PromiseHandle = Rc<RefCell<PromiseData>>;
type ObjectHandle = Rc<RefCell<BTreeMap<String, Value>>>;
type ArrayHandle = Rc<RefCell<Vec<Value>>>;
type NodeListHandle = Rc<Vec<ElementHandle>>;
type MapHandle = Rc<RefCell<MapData>>;
type SetHandle = Rc<RefCell<SetData>>;
type IteratorHandle = Rc<RefCell<IteratorData>>;
type ErrorHandle = Rc<ErrorData>;
type ClassHandle = Rc<ClassDefinition>;
type LocationHandle = Rc<RefCell<AppUrl>>;
type HistoryHandle = Rc<RefCell<HistoryData>>;
type UrlHandle = Rc<RefCell<UrlData>>;
type UrlSearchParamsHandle = Rc<RefCell<UrlSearchParamsData>>;
pub(crate) type StorageHandle = Rc<RefCell<StorageData>>;
type FormDataHandle = Rc<RefCell<FormDataData>>;
type CanvasContextHandle = Rc<RefCell<CanvasContextData>>;

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
    Symbol(String),
    Binding(Rc<RefCell<Value>>, bool),
    Object(ObjectHandle),
    Array(ArrayHandle),
    NodeList(NodeListHandle),
    Map(MapHandle),
    Set(SetHandle),
    Iterator(IteratorHandle),
    Element(ElementHandle),
    Dataset(ElementHandle),
    Error(ErrorHandle),
    Style(ElementHandle),
    ClassList(ElementHandle),
    Event(EventData),
    Response(ResponseHandle),
    Promise(PromiseHandle),
    Document,
    Window,
    Location(LocationHandle),
    ModuleImport(AppUrl),
    History(HistoryHandle),
    Url(UrlHandle),
    UrlSearchParams(UrlSearchParamsHandle),
    Storage(StorageHandle),
    FormData(FormDataHandle),
    CanvasContext(CanvasContextHandle),
    Console,
    Json,
    UrlConstructor,
    UrlSearchParamsConstructor,
    FormDataConstructor,
    MapConstructor,
    SetConstructor,
    ErrorConstructor,
    PromiseConstructor,
    NumberNamespace,
    ArrayNamespace,
    ObjectNamespace,
    StringConstructor,
    SymbolNamespace,
    Class(ClassHandle),
    Function(Rc<Function>),
    Builtin(Builtin),
}

#[derive(Clone)]
enum Builtin {
    DocumentGetElementById,
    DocumentQuerySelector,
    DocumentQuerySelectorAll,
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
    ElementFocus(NodePath),
    ElementBlur(NodePath),
    ElementRequestSubmit(NodePath),
    CanvasGetContext(NodePath),
    CanvasFillRect(CanvasContextHandle),
    CanvasClearRect(CanvasContextHandle),
    UrlSearchParamsAppend(UrlSearchParamsHandle),
    UrlSearchParamsDelete(UrlSearchParamsHandle),
    UrlSearchParamsGet(UrlSearchParamsHandle),
    UrlSearchParamsGetAll(UrlSearchParamsHandle),
    UrlSearchParamsHas(UrlSearchParamsHandle),
    UrlSearchParamsSet(UrlSearchParamsHandle),
    UrlSearchParamsSort(UrlSearchParamsHandle),
    UrlSearchParamsToString(UrlSearchParamsHandle),
    UrlToString(UrlHandle),
    StorageClear(StorageHandle),
    StorageGetItem(StorageHandle),
    StorageKey(StorageHandle),
    StorageRemoveItem(StorageHandle),
    StorageSetItem(StorageHandle),
    FormDataAppend(FormDataHandle),
    FormDataDelete(FormDataHandle),
    FormDataGet(FormDataHandle),
    FormDataGetAll(FormDataHandle),
    FormDataHas(FormDataHandle),
    FormDataSet(FormDataHandle),
    JsonParse,
    JsonStringify,
    SetTimeout,
    ClearTimeout,
    RequestAnimationFrame,
    CancelAnimationFrame,
    CeriQuery,
    ElementAddEventListener(NodePath),
    ElementClick(NodePath),
    ElementAppendChild(NodePath),
    ElementSetAttribute(NodePath),
    ElementGetAttribute(NodePath),
    ElementQuerySelector(NodePath),
    ElementQuerySelectorAll(NodePath),
    ArrayMap(ArrayHandle),
    ArrayJoin(ArrayHandle),
    StringSlice(String),
    MapGet(MapHandle),
    MapSet(MapHandle),
    MapKeys(MapHandle),
    MapValues(MapHandle),
    MapEntries(MapHandle),
    IteratorNext(IteratorHandle),
    IteratorJoin(IteratorHandle),
    SetHas(SetHandle),
    SetValues(SetHandle),
    PromiseReject,
    PromiseResolve,
    PromiseRejectPending(PromiseHandle),
    PromiseResolvePending(PromiseHandle),
    NumberParseInt,
    NumberIsFinite,
    ArrayIsArray,
    ObjectFreeze,
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
    params: Vec<Pattern>,
    body: Vec<Stmt>,
    environment: EnvironmentRef,
    is_async: bool,
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
    Conditional {
        condition: Box<Expr>,
        consequent: Box<Expr>,
        alternate: Box<Expr>,
    },
    Object(Vec<(ObjectLiteralKey, Expr)>),
    Array(Vec<Expr>),
    Spread(Box<Expr>),
    Template(Vec<TemplatePart>),
    Call {
        callee: Box<Expr>,
        arguments: Vec<Expr>,
    },
    New {
        callee: Box<Expr>,
        arguments: Vec<Expr>,
    },
    Function {
        params: Vec<Pattern>,
        body: Vec<Stmt>,
        is_async: bool,
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
enum ObjectLiteralKey {
    Static(String),
    Computed(Expr),
}

#[derive(Clone, Debug, PartialEq)]
enum ObjectPatternKey {
    Static(String),
    Computed(Expr),
}

#[derive(Clone, Debug, PartialEq)]
enum Pattern {
    Identifier(String),
    Default {
        pattern: Box<Pattern>,
        default: Box<Expr>,
    },
    Object {
        properties: Vec<(ObjectPatternKey, Pattern)>,
        rest: Option<Box<Pattern>>,
    },
    Array {
        items: Vec<Option<Pattern>>,
        rest: Option<Box<Pattern>>,
    },
}

#[derive(Clone, Debug, PartialEq)]
enum Stmt {
    Let {
        pattern: Pattern,
        initializer: Option<Expr>,
    },
    Expression(Expr),
    Block(Vec<Stmt>),
    If {
        condition: Expr,
        consequent: Box<Stmt>,
        alternate: Option<Box<Stmt>>,
    },
    ForOf {
        pattern: Pattern,
        iterable: Expr,
        body: Box<Stmt>,
    },
    TryCatch {
        body: Box<Stmt>,
        catch_name: Option<String>,
        handler: Box<Stmt>,
    },
    Throw(Expr),
    Class {
        name: String,
        extends: Option<String>,
    },
    Return(Option<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
enum TemplatePart {
    Text(String),
    Expression(Box<Expr>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ModuleImportBinding {
    Named { imported: String, local: String },
    Namespace { local: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModuleImport {
    pub(crate) source: String,
    pub(crate) bindings: Vec<ModuleImportBinding>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModuleExport {
    pub(crate) local: String,
    pub(crate) exported: String,
}

struct ModuleProgram {
    imports: Vec<ModuleImport>,
    star_reexports: Vec<String>,
    statements: Vec<Stmt>,
    exports: Vec<ModuleExport>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DynamicImportType {
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnaryOperator {
    Not,
    Negative,
    Positive,
    TypeOf,
    Await,
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
    InstanceOf,
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
    focused_element: Option<NodePath>,
    control_values: HashMap<NodePath, String>,
    control_checked: HashMap<NodePath, bool>,
    canvas_contexts: HashMap<NodePath, CanvasContextHandle>,
    canvas_commands: Vec<CanvasCommand>,
    logs: Vec<String>,
    clock: u64,
    next_task_id: u64,
    tasks: Vec<ScheduledTask>,
    microtasks: VecDeque<Microtask>,
    pending_navigation: Option<NavigationRequest>,
    pending_exception: Option<ErrorHandle>,
    host_runtime_info: Option<HostRuntimeInfo>,
    module_cache: BTreeMap<String, ModuleNamespace>,
    loading_modules: BTreeSet<String>,
}

pub(crate) trait ModuleSourceLoader {
    fn load_module(
        &mut self,
        referrer: &AppUrl,
        reference: &str,
    ) -> Result<(AppUrl, String), ScriptError>;
}

struct RuntimeModuleLoader<'a> {
    resources: &'a ResourceBundle,
}

impl ModuleSourceLoader for RuntimeModuleLoader<'_> {
    fn load_module(
        &mut self,
        referrer: &AppUrl,
        reference: &str,
    ) -> Result<(AppUrl, String), ScriptError> {
        let url = referrer
            .resolve(reference)
            .map_err(|error| runtime_error(error.to_string()))?;
        let resource = self
            .resources
            .resource(url.path())
            .ok_or_else(|| runtime_error(format!("module {} was not found", url.path())))?;
        if !is_javascript(resource) {
            return Err(runtime_error(format!(
                "module {} has unsupported MIME type {}",
                url.path(),
                resource.mime_type()
            )));
        }
        let source = String::from_utf8(resource.body().to_vec())
            .map_err(|_| runtime_error(format!("module {} is not UTF-8", url.path())))?;
        Ok((url, source))
    }
}

#[derive(Clone)]
pub(crate) struct ModuleNamespace {
    values: BTreeMap<String, Value>,
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
    key: String,
    data: String,
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

pub(crate) struct StorageData {
    values: BTreeMap<String, String>,
}

impl StorageData {
    pub(crate) fn new() -> Self {
        Self {
            values: BTreeMap::new(),
        }
    }
}

struct FormDataData {
    pairs: Vec<(String, String)>,
}

struct MapData {
    entries: Vec<(Value, Value)>,
}

struct IteratorData {
    values: Vec<Value>,
    index: usize,
}

struct SetData {
    values: Vec<Value>,
}

struct ErrorData {
    name: String,
    message: String,
    code: Option<String>,
    class_name: String,
}

struct ClassDefinition {
    name: String,
    extends: Option<String>,
}

struct CanvasContextData {
    path: NodePath,
    fill_style: Color,
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
const RUNTIME_INFO_REQUEST: &str = r#"{"method":"runtimeInfo","version":1}"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostRuntimeInfo {
    pub engine: String,
    pub chromium: String,
    pub recovery: String,
    pub session: String,
}

impl HostRuntimeInfo {
    pub fn new(
        engine: impl Into<String>,
        chromium: impl Into<String>,
        recovery: impl Into<String>,
        session: impl Into<String>,
    ) -> Self {
        Self {
            engine: engine.into(),
            chromium: chromium.into(),
            recovery: recovery.into(),
            session: session.into(),
        }
    }
}

impl ScriptRuntime {
    pub fn new(document: Rc<RefCell<Document>>) -> Self {
        Self::with_context(document, None, None, None, None, None)
    }

    pub fn with_resources(
        document: Rc<RefCell<Document>>,
        resources: ResourceBundle,
        base_url: AppUrl,
    ) -> Self {
        Self::with_context(document, Some(resources), Some(base_url), None, None, None)
    }

    pub(crate) fn with_resources_and_storage_and_runtime_info(
        document: Rc<RefCell<Document>>,
        resources: ResourceBundle,
        base_url: AppUrl,
        local_storage: StorageHandle,
        session_storage: StorageHandle,
        host_runtime_info: Option<HostRuntimeInfo>,
    ) -> Self {
        Self::with_context(
            document,
            Some(resources),
            Some(base_url),
            Some(local_storage),
            Some(session_storage),
            host_runtime_info,
        )
    }

    pub(crate) fn with_storage_and_runtime_info(
        document: Rc<RefCell<Document>>,
        local_storage: StorageHandle,
        session_storage: StorageHandle,
        host_runtime_info: Option<HostRuntimeInfo>,
    ) -> Self {
        Self::with_context(
            document,
            None,
            None,
            Some(local_storage),
            Some(session_storage),
            host_runtime_info,
        )
    }

    fn with_context(
        document: Rc<RefCell<Document>>,
        resources: Option<ResourceBundle>,
        base_url: Option<AppUrl>,
        local_storage: Option<StorageHandle>,
        session_storage: Option<StorageHandle>,
        host_runtime_info: Option<HostRuntimeInfo>,
    ) -> Self {
        let global = Rc::new(RefCell::new(Environment::default()));
        let local_storage = local_storage
            .unwrap_or_else(|| Rc::new(RefCell::new(StorageData::new())));
        let session_storage = session_storage
            .unwrap_or_else(|| Rc::new(RefCell::new(StorageData::new())));
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
                .insert("Number".to_string(), Value::NumberNamespace);
            values
                .values
                .insert("Array".to_string(), Value::ArrayNamespace);
            values
                .values
                .insert("Object".to_string(), Value::ObjectNamespace);
            values
                .values
                .insert("Promise".to_string(), Value::PromiseConstructor);
            values
                .values
                .insert("Set".to_string(), Value::SetConstructor);
            values
                .values
                .insert("Error".to_string(), Value::ErrorConstructor);
            values
                .values
                .insert("String".to_string(), Value::StringConstructor);
            values
                .values
                .insert("Symbol".to_string(), Value::SymbolNamespace);
            values
                .values
                .insert("URL".to_string(), Value::UrlConstructor);
            values.values.insert(
                "URLSearchParams".to_string(),
                Value::UrlSearchParamsConstructor,
            );
            values
                .values
                .insert("FormData".to_string(), Value::FormDataConstructor);
            values
                .values
                .insert("Map".to_string(), Value::MapConstructor);
            values.values.insert(
                "localStorage".to_string(),
                Value::Storage(local_storage.clone()),
            );
            values.values.insert(
                "sessionStorage".to_string(),
                Value::Storage(session_storage.clone()),
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
            focused_element: None,
            control_values: HashMap::new(),
            control_checked: HashMap::new(),
            canvas_contexts: HashMap::new(),
            canvas_commands: Vec::new(),
            logs: Vec::new(),
            clock: 0,
            next_task_id: 1,
            tasks: Vec::new(),
            microtasks: VecDeque::new(),
            pending_navigation: None,
            pending_exception: None,
            host_runtime_info,
            module_cache: BTreeMap::new(),
            loading_modules: BTreeSet::new(),
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

    pub(crate) fn execute_module<L: ModuleSourceLoader>(
        &mut self,
        source: &str,
        url: AppUrl,
        loader: &mut L,
    ) -> Result<ModuleNamespace, ScriptError> {
        let namespace = self.execute_module_graph(source, url, loader)?;
        self.drain_microtasks()?;
        Ok(namespace)
    }

    fn dynamic_import(
        &mut self,
        base_url: AppUrl,
        reference: &Value,
        attributes: Option<&Value>,
    ) -> Result<ModuleNamespace, ScriptError> {
        let url = base_url
            .resolve(&reference.string_value())
            .map_err(|error| runtime_error(error.to_string()))?;
        let import_type = parse_dynamic_import_type(attributes)?;
        let resources = self
            .resources
            .clone()
            .ok_or_else(|| runtime_error("dynamic import is unavailable without resources"))?;
        let resource = resources
            .resource(url.path())
            .ok_or_else(|| runtime_error(format!("module {} was not found", url.path())))?;
        match import_type {
            None => {
                if !is_javascript(resource) {
                    return Err(runtime_error(format!(
                        "module {} has unsupported MIME type {}",
                        url.path(),
                        resource.mime_type()
                    )));
                }
                let source = String::from_utf8(resource.body().to_vec())
                    .map_err(|_| runtime_error(format!("module {} is not UTF-8", url.path())))?;
                let mut loader = RuntimeModuleLoader {
                    resources: &resources,
                };
                self.execute_module_graph(&source, url, &mut loader)
            }
            Some(DynamicImportType::Json) => {
                if !is_json(resource) {
                    return Err(runtime_error(format!(
                        "module {} has unsupported MIME type {}",
                        url.path(),
                        resource.mime_type()
                    )));
                }
                let source = String::from_utf8(resource.body().to_vec())
                    .map_err(|_| runtime_error(format!("module {} is not UTF-8", url.path())))?;
                let value = self.parse_json(&source)?;
                let mut values = BTreeMap::new();
                values.insert("default".to_string(), value);
                Ok(ModuleNamespace { values })
            }
        }
    }

    fn execute_module_graph<L: ModuleSourceLoader>(
        &mut self,
        source: &str,
        url: AppUrl,
        loader: &mut L,
    ) -> Result<ModuleNamespace, ScriptError> {
        let key = url.to_string();
        if let Some(namespace) = self.module_cache.get(&key) {
            return Ok(namespace.clone());
        }
        if !self.loading_modules.insert(key.clone()) {
            return Err(runtime_error(format!("cyclic module import at {key}")));
        }
        let result = self.execute_module_body(source, url.clone(), loader);
        self.loading_modules.remove(&key);
        let namespace = result?;
        self.module_cache.insert(key, namespace.clone());
        Ok(namespace)
    }

    fn execute_module_body<L: ModuleSourceLoader>(
        &mut self,
        source: &str,
        url: AppUrl,
        loader: &mut L,
    ) -> Result<ModuleNamespace, ScriptError> {
        let program = Parser::new(source)?.parse_module()?;
        let environment = Rc::new(RefCell::new(Environment {
            values: HashMap::new(),
            parent: Some(self.global.clone()),
        }));
        environment
            .borrow_mut()
            .values
            .insert("import".to_string(), Value::ModuleImport(url.clone()));
        {
            let mut values = environment.borrow_mut();
            for export in &program.exports {
                values.values.entry(export.local.clone()).or_insert_with(|| {
                    Value::Binding(Rc::new(RefCell::new(Value::Undefined)), true)
                });
            }
        }
        for import in &program.imports {
            let (dependency_url, dependency_source) = loader.load_module(&url, &import.source)?;
            let namespace = self.execute_module_graph(&dependency_source, dependency_url, loader)?;
            let mut values = environment.borrow_mut();
            for binding in &import.bindings {
                let value = match binding {
                    ModuleImportBinding::Named { imported, .. } => namespace
                        .values
                        .get(imported)
                        .cloned()
                        .ok_or_else(|| {
                            runtime_error(format!(
                                "module {} has no export {imported}",
                                import.source
                            ))
                        })?,
                    ModuleImportBinding::Namespace { .. } => namespace.object_value(),
                };
                let value = match value {
                    Value::Binding(binding, _) => Value::Binding(binding, false),
                    value => value,
                };
                let local = match binding {
                    ModuleImportBinding::Named { local, .. }
                    | ModuleImportBinding::Namespace { local } => local,
                };
                values.values.insert(local.clone(), value);
            }
        }
        let mut star_namespaces = Vec::new();
        for source in &program.star_reexports {
            let (dependency_url, dependency_source) = loader.load_module(&url, source)?;
            star_namespaces.push(self.execute_module_graph(
                &dependency_source,
                dependency_url,
                loader,
            )?);
        }
        match self.execute_statements(&program.statements, environment.clone())? {
            Completion::Normal | Completion::Return(_) => {}
        }
        let values = environment.borrow();
        let mut exports = BTreeMap::new();
        for export in program.exports {
            let value = values
                .get_raw(&export.local)
                .ok_or_else(|| runtime_error(format!("module export {} is undefined", export.local)))?;
            exports.insert(export.exported, value);
        }
        for namespace in star_namespaces {
            for (name, value) in namespace.values {
                if name != "default" {
                    exports.entry(name).or_insert(value);
                }
            }
        }
        Ok(ModuleNamespace { values: exports })
    }

    pub fn click_id(&mut self, id: &str) -> Result<bool, ScriptError> {
        let Some(path) = self.document.borrow().find_element_by_id(id) else {
            return Ok(false);
        };
        self.click_path(path)
    }

    pub fn click_path(&mut self, path: NodePath) -> Result<bool, ScriptError> {
        if self
            .document
            .borrow()
            .element_clone(&path)
            .is_some_and(|element| element.attrs.contains_key("disabled"))
        {
            return Ok(false);
        }
        let focused = self.focus_path(path.clone())?;
        let dispatch = self.dispatch_event(path.clone(), "click")?;
        self.drain_microtasks()?;
        if !dispatch.default_prevented && self.pending_navigation.is_none() {
            self.apply_click_default(&path)?;
            if let Some(form) = self.submit_form_for_click(&path) {
                self.request_submit(form)?;
            }
            let reference = self.document.borrow().link_reference(&path);
            if self.pending_navigation.is_none()
                && let Some(reference) = reference
            {
                self.request_location_navigation(reference, false);
            }
        }
        Ok(dispatch.handled || focused)
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

    pub(crate) fn focus_path(&mut self, path: NodePath) -> Result<bool, ScriptError> {
        if !self.is_focusable_path(&path) || self.focused_element.as_ref() == Some(&path) {
            return Ok(false);
        }
        let previous = self.focused_element.replace(path.clone());
        if let Some(previous) = previous {
            self.dispatch_event(previous, "blur")?;
            self.drain_microtasks()?;
        }
        self.dispatch_event(path, "focus")?;
        self.drain_microtasks()?;
        Ok(true)
    }

    pub(crate) fn blur_focused(&mut self) -> Result<bool, ScriptError> {
        let Some(path) = self.focused_element.take() else {
            return Ok(false);
        };
        self.dispatch_event(path, "blur")?;
        self.drain_microtasks()?;
        Ok(true)
    }

    pub(crate) fn insert_text(&mut self, text: &str) -> Result<bool, ScriptError> {
        if text.is_empty() {
            return Ok(false);
        }
        let Some(path) = self.focused_element.clone() else {
            return Ok(false);
        };
        if !self.is_text_control_path(&path) {
            return Ok(false);
        }
        let mut value = self.control_value_path(&path).unwrap_or_default();
        value.push_str(text);
        self.control_values.insert(path.clone(), value);
        let dispatch = self.dispatch_event_with_details(path, "input", "", text)?;
        self.drain_microtasks()?;
        Ok(dispatch.handled || !text.is_empty())
    }

    pub(crate) fn key_down(&mut self, key: &str) -> Result<bool, ScriptError> {
        let Some(path) = self.focused_element.clone() else {
            return Ok(false);
        };
        let dispatch = self.dispatch_event_with_details(path.clone(), "keydown", key, "")?;
        self.drain_microtasks()?;
        if dispatch.default_prevented || !self.is_text_control_path(&path) {
            return Ok(dispatch.handled);
        }
        if key == "Backspace" {
            let mut value = self.control_value_path(&path).unwrap_or_default();
            if value.pop().is_some() {
                self.control_values.insert(path.clone(), value);
                self.dispatch_event_with_details(path, "input", key, "")?;
                self.drain_microtasks()?;
                return Ok(true);
            }
        }
        Ok(dispatch.handled)
    }

    pub fn logs(&self) -> &[String] {
        &self.logs
    }

    pub(crate) fn canvas_commands(&self) -> &[CanvasCommand] {
        &self.canvas_commands
    }

    pub(crate) fn style_state(&self, hovered_path: Option<NodePath>) -> StyleState {
        let mut state = StyleState {
            hovered_path,
            focused_path: self.focused_element.clone(),
            ..StyleState::default()
        };
        for (path, checked) in &self.control_checked {
            if *checked {
                state.checked_paths.insert(path.clone());
            } else {
                state.unchecked_paths.insert(path.clone());
            }
        }
        state
    }

    pub fn document(&self) -> Rc<RefCell<Document>> {
        self.document.clone()
    }

    pub fn now(&self) -> u64 {
        self.clock
    }

    fn apply_click_default(&mut self, path: &[usize]) -> Result<(), ScriptError> {
        let Some(element) = self.document.borrow().element_clone(path) else {
            return Ok(());
        };
        if element.name != "input" {
            return Ok(());
        }
        let input_type = element
            .attr("type")
            .map(|kind| kind.to_ascii_lowercase())
            .unwrap_or_default();
        let changed = match input_type.as_str() {
            "checkbox" => {
                let checked = self.control_checked_path(path);
                self.control_checked.insert(path.to_vec(), !checked);
                true
            }
            "radio" => {
                let was_checked = self.control_checked_path(path);
                if let Some(group) = self.document.borrow().radio_group_paths(path) {
                    for candidate in group {
                        if candidate != path {
                            self.control_checked.insert(candidate, false);
                        }
                    }
                }
                self.control_checked.insert(path.to_vec(), true);
                !was_checked
            }
            _ => false,
        };
        if changed {
            self.dispatch_event(path.to_vec(), "input")?;
            self.drain_microtasks()?;
            self.dispatch_event(path.to_vec(), "change")?;
            self.drain_microtasks()?;
        }
        Ok(())
    }

    fn submit_form_for_click(&self, path: &[usize]) -> Option<NodePath> {
        let element = self.document.borrow().element_clone(path)?;
        let input_type = element
            .attr("type")
            .map(|kind| kind.to_ascii_lowercase())
            .unwrap_or_default();
        let is_submit = match element.name.as_str() {
            "button" => !matches!(input_type.as_str(), "button" | "reset"),
            "input" => input_type == "submit",
            _ => false,
        };
        is_submit
            .then(|| self.document.borrow().form_ancestor_path(path))
            .flatten()
    }

    fn request_submit(&mut self, path: NodePath) -> Result<(), ScriptError> {
        let dispatch = self.dispatch_event(path.clone(), "submit")?;
        self.drain_microtasks()?;
        if !dispatch.default_prevented {
            self.submit_form(path)?;
        }
        Ok(())
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

    pub(crate) fn active_element(&self) -> Option<NodePath> {
        self.focused_element.clone()
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
            key: String::new(),
            data: String::new(),
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

    fn bind_pattern(
        &mut self,
        pattern: &Pattern,
        value: Value,
        environment: &EnvironmentRef,
    ) -> Result<(), ScriptError> {
        match pattern {
            Pattern::Identifier(name) => {
                let existing = environment.borrow().values.get(name).cloned();
                if let Some(Value::Binding(binding, true)) = existing {
                    *binding.borrow_mut() = value;
                } else {
                    environment
                        .borrow_mut()
                        .values
                        .insert(name.clone(), value);
                }
                Ok(())
            }
            Pattern::Default { pattern, default } => {
                let value = if matches!(&value, Value::Undefined) {
                    self.evaluate(default, environment.clone())?
                } else {
                    value
                };
                self.bind_pattern(pattern, value, environment)
            }
            Pattern::Object { properties, rest } => {
                if matches!(value, Value::Null | Value::Undefined) {
                    return Err(runtime_error("cannot destructure a nullish value"));
                }
                let mut consumed = BTreeSet::new();
                let mut object_properties = BTreeMap::new();
                if let Value::Object(object) = &value {
                    object_properties = object.borrow().clone();
                }
                for (key, nested) in properties {
                    let property = match key {
                        ObjectPatternKey::Static(property) => property.clone(),
                        ObjectPatternKey::Computed(expression) => self
                            .evaluate(expression, environment.clone())?
                            .string_value(),
                    };
                    consumed.insert(property.clone());
                    let nested_value = if let Some(nested_value) = object_properties.get(&property) {
                        nested_value.clone()
                    } else {
                        self.member(value.clone(), &property)
                            .unwrap_or(Value::Undefined)
                    };
                    self.bind_pattern(nested, nested_value, environment)?;
                }
                if let Some(rest) = rest {
                    let remaining = object_properties
                        .into_iter()
                        .filter(|(property, _)| !consumed.contains(property))
                        .collect();
                    self.bind_pattern(
                        rest,
                        Value::Object(Rc::new(RefCell::new(remaining))),
                        environment,
                    )?;
                }
                Ok(())
            }
            Pattern::Array { items, rest } => {
                let values = self.iterable_values(value)?;
                for (index, nested) in items.iter().enumerate() {
                    let Some(nested) = nested else {
                        continue;
                    };
                    self.bind_pattern(
                        nested,
                        values.get(index).cloned().unwrap_or(Value::Undefined),
                        environment,
                    )?;
                }
                if let Some(rest) = rest {
                    let remaining = values.into_iter().skip(items.len()).collect();
                    self.bind_pattern(
                        rest,
                        Value::Array(Rc::new(RefCell::new(remaining))),
                        environment,
                    )?;
                }
                Ok(())
            }
        }
    }

    fn execute_statement(
        &mut self,
        statement: &Stmt,
        environment: EnvironmentRef,
    ) -> Result<Completion, ScriptError> {
        match statement {
            Stmt::Let {
                pattern,
                initializer,
            } => {
                let value = initializer
                    .as_ref()
                    .map(|expression| self.evaluate(expression, environment.clone()))
                    .transpose()?
                    .unwrap_or(Value::Undefined);
                self.bind_pattern(pattern, value, &environment)?;
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
            Stmt::ForOf {
                pattern,
                iterable,
                body,
            } => {
                let iterable = self.evaluate(iterable, environment.clone())?;
                let values = self.iterable_values(iterable)?;
                for value in values {
                    let iteration = Rc::new(RefCell::new(Environment {
                        values: HashMap::new(),
                        parent: Some(environment.clone()),
                    }));
                    self.bind_pattern(pattern, value, &iteration)?;
                    let completion = self.execute_statement(body, iteration)?;
                    if !matches!(completion, Completion::Normal) {
                        return Ok(completion);
                    }
                }
                Ok(Completion::Normal)
            }
            Stmt::TryCatch {
                body,
                catch_name,
                handler,
            } => match self.execute_statement(body, environment.clone()) {
                Ok(completion) => Ok(completion),
                Err(error) => {
                    let error_value = self
                        .pending_exception
                        .take()
                        .map(Value::Error)
                        .unwrap_or_else(|| {
                            Value::Error(Rc::new(ErrorData {
                                name: "Error".to_string(),
                                message: error.message.clone(),
                                code: None,
                                class_name: "Error".to_string(),
                            }))
                        });
                    let handler_environment = Rc::new(RefCell::new(Environment {
                        values: HashMap::new(),
                        parent: Some(environment),
                    }));
                    if let Some(name) = catch_name {
                        handler_environment
                            .borrow_mut()
                            .values
                            .insert(name.clone(), error_value);
                    }
                    self.execute_statement(handler, handler_environment)
                }
            },
            Stmt::Throw(expression) => {
                let value = self.evaluate(expression, environment)?;
                let error = error_handle(&value);
                let message = error.message.clone();
                self.pending_exception = Some(error);
                Err(runtime_error(message))
            }
            Stmt::Class { name, extends } => {
                let class = Value::Class(Rc::new(ClassDefinition {
                    name: name.clone(),
                    extends: extends.clone(),
                }));
                let existing = environment.borrow().values.get(name).cloned();
                if let Some(Value::Binding(binding, true)) = existing {
                    *binding.borrow_mut() = class;
                } else {
                    environment.borrow_mut().values.insert(name.clone(), class);
                }
                Ok(Completion::Normal)
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
                    UnaryOperator::TypeOf => Ok(Value::String(value_typeof(&value).to_string())),
                    UnaryOperator::Await => self.await_value(value),
                }
            }
            Expr::Conditional {
                condition,
                consequent,
                alternate,
            } => {
                if self.evaluate(condition, environment.clone())?.truthy() {
                    self.evaluate(consequent, environment)
                } else {
                    self.evaluate(alternate, environment)
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
                for (key, expression) in properties {
                    let property = match key {
                        ObjectLiteralKey::Static(property) => property.clone(),
                        ObjectLiteralKey::Computed(key_expression) => self
                            .evaluate(key_expression, environment.clone())
                            .map(|value| value.property_key())?,
                    };
                    object.insert(
                        property,
                        self.evaluate(expression, environment.clone())?,
                    );
                }
                Ok(Value::Object(Rc::new(RefCell::new(object))))
            }
            Expr::Array(expressions) => {
                let mut values = Vec::new();
                for expression in expressions {
                    if let Expr::Spread(expression) = expression {
                        let spread = self.evaluate(expression, environment.clone())?;
                        values.extend(self.spread_values(spread)?);
                    } else {
                        values.push(self.evaluate(expression, environment.clone())?);
                    }
                }
                Ok(Value::Array(Rc::new(RefCell::new(values))))
            }
            Expr::Spread(_) => Err(runtime_error("spread is only valid in an array literal")),
            Expr::Template(parts) => {
                let mut output = String::new();
                for part in parts {
                    match part {
                        TemplatePart::Text(text) => output.push_str(text),
                        TemplatePart::Expression(expression) => output.push_str(
                            &self
                                .evaluate(expression, environment.clone())?
                                .string_value(),
                        ),
                    }
                }
                Ok(Value::String(output))
            }
            Expr::Call { callee, arguments } => {
                if matches!(callee.as_ref(), Expr::Identifier(name) if name == "import") {
                    let base_url = environment
                        .borrow()
                        .get("import")
                        .and_then(|value| match value {
                            Value::ModuleImport(url) => Some(url),
                            _ => None,
                        })
                        .or_else(|| self.base_url.clone());
                    let outcome = match (base_url, arguments.first()) {
                        (Some(base_url), Some(argument)) => {
                            if arguments.len() > 2 {
                                Err(runtime_error(
                                    "dynamic import accepts at most two arguments",
                                ))
                            } else {
                                self.evaluate(argument, environment.clone()).and_then(
                                    |reference| {
                                        let attributes = arguments
                                            .get(1)
                                            .map(|argument| {
                                                self.evaluate(argument, environment.clone())
                                            })
                                            .transpose()?;
                                        let attributes = attributes.as_ref().filter(|value| {
                                            !matches!(value, Value::Undefined)
                                        });
                                        self.dynamic_import(
                                            base_url,
                                            &reference,
                                            attributes,
                                        )
                                    },
                                )
                            }
                        }
                        (None, _) => Err(runtime_error(
                            "dynamic import requires a document URL",
                        )),
                        (_, None) => Err(runtime_error("dynamic import requires a specifier")),
                    };
                    return Ok(Value::Promise(match outcome {
                        Ok(namespace) => self.fulfilled_promise(namespace.object_value()),
                        Err(error) => self.rejected_promise(error.message),
                    }));
                }
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
            Expr::Function {
                params,
                body,
                is_async,
            } => Ok(Value::Function(Rc::new(Function {
                params: params.clone(),
                body: body.clone(),
                environment,
                is_async: *is_async,
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
                if !environment.borrow_mut().assign(name, value.clone())? {
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
        match object.resolve() {
            Value::Document => match property {
                "getElementById" => Ok(Value::Builtin(Builtin::DocumentGetElementById)),
                "querySelector" => Ok(Value::Builtin(Builtin::DocumentQuerySelector)),
                "querySelectorAll" => Ok(Value::Builtin(Builtin::DocumentQuerySelectorAll)),
                "createElement" => Ok(Value::Builtin(Builtin::DocumentCreateElement)),
                "documentElement" => Ok(self
                    .document
                    .borrow()
                    .document_element_path()
                    .map(|path| Value::Element(attached_handle(path)))
                    .unwrap_or(Value::Null)),
                "activeElement" => Ok(self
                    .active_element()
                    .map(attached_handle)
                    .map(Value::Element)
                    .unwrap_or(Value::Null)),
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
                "ceriQuery" => Ok(self
                    .host_runtime_info
                    .as_ref()
                    .map(|_| Value::Builtin(Builtin::CeriQuery))
                    .unwrap_or(Value::Undefined)),
                "cefQuery" | "cefQueryCancel" => Ok(Value::Undefined),
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
                    "value" => self
                        .element_value(&handle)
                        .map(Value::String)
                        .unwrap_or_else(|| Value::String(String::new())),
                    "checked" => Value::Bool(self.element_checked(&handle)),
                    "disabled" => Value::Bool(
                        self.element_attribute(&handle, "disabled").is_some(),
                    ),
                    "dataset" => Value::Dataset(handle.clone()),
                    "style" => Value::Style(handle),
                    "classList" => Value::ClassList(handle),
                    "querySelector" => {
                        Value::Builtin(Builtin::ElementQuerySelector(path.ok_or_else(|| {
                            runtime_error("detached element cannot query descendants")
                        })?))
                    }
                    "querySelectorAll" => Value::Builtin(Builtin::ElementQuerySelectorAll(
                        path.ok_or_else(|| {
                            runtime_error("detached element cannot query descendants")
                        })?,
                    )),
                    "addEventListener" => {
                        Value::Builtin(Builtin::ElementAddEventListener(path.ok_or_else(|| {
                            runtime_error("detached element cannot receive events")
                        })?))
                    }
                    "click" => Value::Builtin(Builtin::ElementClick(
                        path.ok_or_else(|| runtime_error("detached element cannot be clicked"))?,
                    )),
                    "focus" => Value::Builtin(Builtin::ElementFocus(
                        path.ok_or_else(|| runtime_error("detached element cannot receive focus"))?,
                    )),
                    "blur" => Value::Builtin(Builtin::ElementBlur(
                        path.ok_or_else(|| runtime_error("detached element cannot lose focus"))?,
                    )),
                    "requestSubmit" => {
                        let path = path.ok_or_else(|| {
                            runtime_error("detached element cannot submit")
                        })?;
                        let is_form = self
                            .document
                            .borrow()
                            .element_clone(&path)
                            .is_some_and(|element| element.name == "form");
                        if !is_form {
                            return Err(runtime_error(
                                "requestSubmit is only available on form elements",
                            ));
                        }
                        Value::Builtin(Builtin::ElementRequestSubmit(path))
                    }
                    "getContext" => {
                        let path = path.ok_or_else(|| {
                            runtime_error("detached element cannot create a canvas context")
                        })?;
                        let is_canvas = self
                            .document
                            .borrow()
                            .element_clone(&path)
                            .is_some_and(|element| element.name == "canvas");
                        if !is_canvas {
                            return Err(runtime_error(
                                "getContext is only available on canvas elements",
                            ));
                        }
                        Value::Builtin(Builtin::CanvasGetContext(path))
                    }
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
                "key" => Ok(Value::String(event.key.clone())),
                "data" => Ok(Value::String(event.data.clone())),
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
            Value::ModuleImport(url) if property == "meta" => {
                let mut meta = BTreeMap::new();
                meta.insert(
                    "url".to_string(),
                    Value::String(url.to_string()),
                );
                Ok(Value::Object(Rc::new(RefCell::new(meta))))
            }
            Value::ModuleImport(_) => Err(runtime_error(format!(
                "import has no property {property}"
            ))),
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
            Value::Storage(storage) => match property {
                "length" => Ok(Value::Number(storage.borrow().values.len() as f64)),
                "key" => Ok(Value::Builtin(Builtin::StorageKey(storage))),
                "getItem" => Ok(Value::Builtin(Builtin::StorageGetItem(storage))),
                "setItem" => Ok(Value::Builtin(Builtin::StorageSetItem(storage))),
                "removeItem" => Ok(Value::Builtin(Builtin::StorageRemoveItem(storage))),
                "clear" => Ok(Value::Builtin(Builtin::StorageClear(storage))),
                _ => Err(runtime_error(format!("storage has no property {property}"))),
            },
            Value::FormData(form_data) => match property {
                "append" => Ok(Value::Builtin(Builtin::FormDataAppend(form_data))),
                "delete" => Ok(Value::Builtin(Builtin::FormDataDelete(form_data))),
                "get" => Ok(Value::Builtin(Builtin::FormDataGet(form_data))),
                "getAll" => Ok(Value::Builtin(Builtin::FormDataGetAll(form_data))),
                "has" => Ok(Value::Builtin(Builtin::FormDataHas(form_data))),
                "set" => Ok(Value::Builtin(Builtin::FormDataSet(form_data))),
                _ => Err(runtime_error(format!("FormData has no property {property}"))),
            },
            Value::CanvasContext(context) => match property {
                "fillStyle" => Ok(Value::String(color_css(context.borrow().fill_style))),
                "fillRect" => Ok(Value::Builtin(Builtin::CanvasFillRect(context))),
                "clearRect" => Ok(Value::Builtin(Builtin::CanvasClearRect(context))),
                _ => Err(runtime_error(format!(
                    "CanvasRenderingContext2D has no property {property}"
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
            Value::String(string) => match property {
                "length" => Ok(Value::Number(string.chars().count() as f64)),
                "slice" => Ok(Value::Builtin(Builtin::StringSlice(string))),
                _ => Err(runtime_error(format!("string has no property {property}"))),
            },
            Value::Set(set) => match property {
                "size" => Ok(Value::Number(set.borrow().values.len() as f64)),
                "has" => Ok(Value::Builtin(Builtin::SetHas(set))),
                "values" | "keys" => Ok(Value::Builtin(Builtin::SetValues(set))),
                _ => Err(runtime_error(format!("Set has no property {property}"))),
            },
            Value::Iterator(iterator) => match property {
                "length" => {
                    let iterator = iterator.borrow();
                    Ok(Value::Number(
                        iterator.values.len().saturating_sub(iterator.index) as f64,
                    ))
                }
                "next" => Ok(Value::Builtin(Builtin::IteratorNext(iterator))),
                "join" => Ok(Value::Builtin(Builtin::IteratorJoin(iterator))),
                _ => Err(runtime_error(format!("iterator has no property {property}"))),
            },
            Value::Error(error) => match property {
                "name" => Ok(Value::String(error.name.clone())),
                "message" => Ok(Value::String(error.message.clone())),
                "code" => Ok(error
                    .code
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Undefined)),
                _ => Err(runtime_error(format!("error has no property {property}"))),
            },
            Value::PromiseConstructor => match property {
                "reject" => Ok(Value::Builtin(Builtin::PromiseReject)),
                "resolve" => Ok(Value::Builtin(Builtin::PromiseResolve)),
                _ => Err(runtime_error(format!("Promise has no property {property}"))),
            },
            Value::NumberNamespace => match property {
                "parseInt" => Ok(Value::Builtin(Builtin::NumberParseInt)),
                "isFinite" => Ok(Value::Builtin(Builtin::NumberIsFinite)),
                _ => Err(runtime_error(format!("Number has no property {property}"))),
            },
            Value::ArrayNamespace => match property {
                "isArray" => Ok(Value::Builtin(Builtin::ArrayIsArray)),
                _ => Err(runtime_error(format!("Array has no property {property}"))),
            },
            Value::ObjectNamespace => match property {
                "freeze" => Ok(Value::Builtin(Builtin::ObjectFreeze)),
                _ => Err(runtime_error(format!("Object has no property {property}"))),
            },
            Value::SymbolNamespace => match property {
                "iterator" => Ok(Value::Symbol("@@iterator".to_string())),
                _ => Err(runtime_error(format!("Symbol has no property {property}"))),
            },
            Value::Object(object) => Ok(object
                .borrow()
                .get(property)
                .map(Value::resolve)
                .unwrap_or(Value::Undefined)),
            Value::Array(array) => {
                match property {
                    "length" => Ok(Value::Number(array.borrow().len() as f64)),
                    "map" => Ok(Value::Builtin(Builtin::ArrayMap(array))),
                    "join" => Ok(Value::Builtin(Builtin::ArrayJoin(array))),
                    _ => {
                        let index = property.parse::<usize>().map_err(|_| {
                            runtime_error(format!("array has no property {property}"))
                        })?;
                        Ok(array
                            .borrow()
                            .get(index)
                            .cloned()
                            .unwrap_or(Value::Undefined))
                    }
                }
            }
            Value::NodeList(nodes) => {
                if property == "length" {
                    Ok(Value::Number(nodes.len() as f64))
                } else {
                    let index = property.parse::<usize>().map_err(|_| {
                        runtime_error(format!("NodeList has no property {property}"))
                    })?;
                    Ok(nodes
                        .get(index)
                        .cloned()
                        .map(Value::Element)
                        .unwrap_or(Value::Undefined))
                }
            }
            Value::Dataset(handle) => Ok(self
                .element_attribute(&handle, &dataset_attribute_name(property))
                .map(Value::String)
                .unwrap_or(Value::Undefined)),
            Value::Map(map) => match property {
                "size" => Ok(Value::Number(map.borrow().entries.len() as f64)),
                "get" => Ok(Value::Builtin(Builtin::MapGet(map))),
                "set" => Ok(Value::Builtin(Builtin::MapSet(map))),
                "keys" => Ok(Value::Builtin(Builtin::MapKeys(map))),
                "values" => Ok(Value::Builtin(Builtin::MapValues(map))),
                "entries" => Ok(Value::Builtin(Builtin::MapEntries(map))),
                _ => Err(runtime_error(format!("Map has no property {property}"))),
            },
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
            object => Err(runtime_error(format!(
                "{} value has no property {property}",
                value_typeof(&object)
            ))),
        }
    }

    fn index(&self, object: Value, index: Value) -> Result<Value, ScriptError> {
        match object.resolve() {
            Value::Object(object) => Ok(object
                .borrow()
                .get(&index.property_key())
                .map(Value::resolve)
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
            Value::NodeList(nodes) => {
                let index = index.number()?;
                if !index.is_finite() || index < 0.0 || index.fract() != 0.0 {
                    return Ok(Value::Undefined);
                }
                Ok(nodes
                    .get(index as usize)
                    .cloned()
                    .map(Value::Element)
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
        let truthy = value.truthy();
        let value = value.string_value();
        let updated = match object.resolve() {
            Value::Element(handle) => match property {
                "textContent" => self.set_element_text_content(&handle, value),
                "className" => self.set_element_attribute(&handle, "class", value),
                "id" => self.set_element_attribute(&handle, "id", value),
                "innerHTML" => self.set_element_inner_html(&handle, value)?,
                "value" => self.set_element_value(&handle, value),
                "checked" => self.set_element_checked(&handle, truthy),
                "disabled" => self.set_element_disabled(&handle, truthy),
                _ => false,
            },
            Value::Dataset(handle) => {
                self.set_element_attribute(&handle, &dataset_attribute_name(property), value)
            }
            Value::Style(handle) => self.set_element_style_property(&handle, property, value),
            Value::CanvasContext(context) if property == "fillStyle" => {
                self.set_canvas_fill_style(&context, &value)
            }
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

    fn is_focusable_path(&self, path: &[usize]) -> bool {
        let Some(element) = self.document.borrow().element_clone(path) else {
            return false;
        };
        if element.attr("disabled").is_some() {
            return false;
        }
        if element.attr("tabindex").is_some() {
            return true;
        }
        match element.name.as_str() {
            "button" | "select" | "textarea" => true,
            "input" => !element
                .attr("type")
                .is_some_and(|kind| kind.eq_ignore_ascii_case("hidden")),
            "a" => element.attr("href").is_some(),
            _ => false,
        }
    }

    fn is_text_control_path(&self, path: &[usize]) -> bool {
        let Some(element) = self.document.borrow().element_clone(path) else {
            return false;
        };
        match element.name.as_str() {
            "textarea" => true,
            "input" => !element.attr("type").is_some_and(|kind| {
                matches!(
                    kind.to_ascii_lowercase().as_str(),
                    "button" | "checkbox" | "color" | "file" | "hidden" | "radio" | "range" | "reset" | "submit"
                )
            }),
            _ => false,
        }
    }

    fn control_value_path(&self, path: &[usize]) -> Option<String> {
        if let Some(value) = self.control_values.get(path) {
            return Some(value.clone());
        }
        let element = self.document.borrow().element_clone(path)?;
        element.attr("value").map(str::to_string).or_else(|| {
            (element.name == "textarea").then(|| {
                let mut text = String::new();
                collect_element_text(&element, &mut text);
                text
            })
        })
    }

    fn element_value(&self, handle: &ElementHandle) -> Option<String> {
        match handle.borrow().clone() {
            ElementLocation::Attached(path) => self.control_value_path(&path),
            ElementLocation::Detached(element) => element.attr("value").map(str::to_string),
        }
    }

    fn element_checked(&self, handle: &ElementHandle) -> bool {
        match handle.borrow().clone() {
            ElementLocation::Attached(path) => self.control_checked_path(&path),
            ElementLocation::Detached(element) => element.attr("checked").is_some(),
        }
    }

    fn set_element_value(&mut self, handle: &ElementHandle, value: String) -> bool {
        match handle.borrow().clone() {
            ElementLocation::Attached(path) => {
                self.control_values.insert(path, value);
                true
            }
            ElementLocation::Detached(_) => {
                let mut location = handle.borrow_mut();
                let ElementLocation::Detached(element) = &mut *location else {
                    return false;
                };
                element.attrs.insert("value".to_string(), value);
                true
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
            ElementLocation::Attached(path) => {
                let changed = self
                    .document
                    .borrow_mut()
                    .set_element_attribute(&path, name, value);
                if changed && name.eq_ignore_ascii_case("checked") {
                    self.control_checked.insert(path, true);
                }
                changed
            }
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
        match callee.resolve() {
            Value::Function(function) => {
                let environment = Rc::new(RefCell::new(Environment {
                    values: HashMap::new(),
                    parent: Some(function.environment.clone()),
                }));
                for (index, parameter) in function.params.iter().enumerate() {
                    self.bind_pattern(
                        parameter,
                        arguments.get(index).cloned().unwrap_or(Value::Undefined),
                        &environment,
                    )?;
                }
                let outcome = self.execute_statements(&function.body, environment);
                if function.is_async {
                    match outcome {
                        Ok(Completion::Normal) => {
                            Ok(Value::Promise(self.fulfilled_promise(Value::Undefined)))
                        }
                        Ok(Completion::Return(value)) => {
                            Ok(Value::Promise(self.fulfilled_promise(value)))
                        }
                        Err(error) => {
                            let reason = self
                                .pending_exception
                                .take()
                                .map(Value::Error)
                                .unwrap_or_else(|| Value::String(error.message));
                            Ok(Value::Promise(self.rejected_promise_value(reason)))
                        }
                    }
                } else {
                    match outcome? {
                        Completion::Normal => Ok(Value::Undefined),
                        Completion::Return(value) => Ok(value),
                    }
                }
            }
            Value::Builtin(builtin) => self.call_builtin(builtin, arguments),
            Value::UrlConstructor => self.construct_url(arguments),
            Value::UrlSearchParamsConstructor => self.construct_url_search_params(arguments),
            Value::FormDataConstructor => self.construct_form_data(arguments),
            Value::MapConstructor => self.construct_map(arguments),
            Value::SetConstructor => self.construct_set(arguments),
            Value::ErrorConstructor => Ok(self.construct_error(arguments)),
            Value::PromiseConstructor => self.construct_promise(arguments),
            Value::StringConstructor => Ok(Value::String(
                arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default(),
            )),
            Value::Class(class) => self.construct_class(class, arguments),
            _ => Err(runtime_error("value is not callable")),
        }
    }

    fn await_value(&mut self, value: Value) -> Result<Value, ScriptError> {
        let Value::Promise(promise) = value else {
            return Ok(value);
        };
        loop {
            let outcome = promise.borrow().outcome.clone();
            match outcome {
                PromiseOutcome::Fulfilled(value) => return Ok(value),
                PromiseOutcome::Rejected(reason) => {
                    let error = error_handle(&reason);
                    let message = error.message.clone();
                    self.pending_exception = Some(error);
                    return Err(runtime_error(message));
                }
                PromiseOutcome::Pending => {
                    self.drain_microtasks()?;
                    if matches!(promise.borrow().outcome, PromiseOutcome::Pending) {
                        return Err(runtime_error("awaited promise is still pending"));
                    }
                }
            }
        }
    }

    fn set_canvas_fill_style(&mut self, context: &CanvasContextHandle, value: &str) -> bool {
        let Some(color) = Color::parse(value) else {
            return false;
        };
        context.borrow_mut().fill_style = color;
        true
    }

    fn set_element_checked(&mut self, handle: &ElementHandle, checked: bool) -> bool {
        match handle.borrow().clone() {
            ElementLocation::Attached(path) => {
                self.control_checked.insert(path, checked);
                true
            }
            ElementLocation::Detached(_) => {
                let mut location = handle.borrow_mut();
                let ElementLocation::Detached(element) = &mut *location else {
                    return false;
                };
                if checked {
                    element.attrs.insert("checked".to_string(), String::new());
                } else {
                    element.attrs.remove("checked");
                }
                true
            }
        }
    }

    fn set_element_disabled(&mut self, handle: &ElementHandle, disabled: bool) -> bool {
        match handle.borrow().clone() {
            ElementLocation::Attached(path) => {
                if disabled {
                    self.document
                        .borrow_mut()
                        .set_element_attribute(&path, "disabled", String::new())
                } else {
                    self.document
                        .borrow_mut()
                        .remove_element_attribute(&path, "disabled")
                }
            }
            ElementLocation::Detached(_) => {
                let mut location = handle.borrow_mut();
                let ElementLocation::Detached(element) = &mut *location else {
                    return false;
                };
                if disabled {
                    element.attrs.insert("disabled".to_string(), String::new());
                } else {
                    element.attrs.remove("disabled");
                }
                true
            }
        }
    }

    fn control_checked_path(&self, path: &[usize]) -> bool {
        if let Some(checked) = self.control_checked.get(path) {
            return *checked;
        }
        self.document
            .borrow()
            .element_clone(path)
            .is_some_and(|element| element.attr("checked").is_some())
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

    fn construct_form_data(&self, arguments: Vec<Value>) -> Result<Value, ScriptError> {
        let Some(form) = arguments.first() else {
            return Ok(Value::FormData(Rc::new(RefCell::new(FormDataData {
                pairs: Vec::new(),
            }))));
        };
        let Value::Element(handle) = form else {
            return Err(runtime_error("FormData constructor requires a form element"));
        };
        let path = attached_path(handle)
            .ok_or_else(|| runtime_error("FormData constructor requires an attached form"))?;
        let paths = self
            .document
            .borrow()
            .form_control_paths(&path)
            .ok_or_else(|| runtime_error("FormData constructor requires a form element"))?;
        let mut pairs = Vec::new();
        for path in paths {
            let element = self
                .document
                .borrow()
                .element_clone(&path)
                .ok_or_else(|| runtime_error("form control is no longer in the document"))?;
            let Some(name) = element.attr("name").filter(|name| !name.is_empty()) else {
                continue;
            };
            if element.attr("disabled").is_some() {
                continue;
            }
            let input_type = element
                .attr("type")
                .map(|kind| kind.to_ascii_lowercase())
                .unwrap_or_default();
            if matches!(input_type.as_str(), "button" | "file" | "reset" | "submit") {
                continue;
            }
            if matches!(input_type.as_str(), "checkbox" | "radio")
                && !self.control_checked_path(&path)
            {
                continue;
            }
            let value = self
                .control_value_path(&path)
                .or_else(|| element.attr("value").map(str::to_string))
                .unwrap_or_default();
            pairs.push((name.to_string(), value));
        }
        Ok(Value::FormData(Rc::new(RefCell::new(FormDataData { pairs }))))
    }

    fn construct_map(&self, arguments: Vec<Value>) -> Result<Value, ScriptError> {
        let mut entries = Vec::new();
        if let Some(Value::Array(sequence)) = arguments.first() {
            for entry in sequence.borrow().iter() {
                let Value::Array(entry) = entry else {
                    return Err(runtime_error("Map sequence entries must be arrays"));
                };
                let entry = entry.borrow();
                if entry.len() < 2 {
                    return Err(runtime_error("Map sequence entries need two values"));
                }
                entries.push((entry[0].clone(), entry[1].clone()));
            }
        } else if !arguments.is_empty() {
            return Err(runtime_error("Map constructor requires an array sequence"));
        }
        Ok(Value::Map(Rc::new(RefCell::new(MapData { entries }))))
    }

    fn construct_set(&mut self, arguments: Vec<Value>) -> Result<Value, ScriptError> {
        let mut values = Vec::new();
        if let Some(value) = arguments.first() {
            for candidate in self.iterable_values(value.clone())? {
                if !values.iter().any(|existing| values_equal(existing, &candidate)) {
                    values.push(candidate);
                }
            }
        }
        Ok(Value::Set(Rc::new(RefCell::new(SetData { values }))))
    }

    fn construct_error(&self, arguments: Vec<Value>) -> Value {
        Value::Error(Rc::new(ErrorData {
            name: "Error".to_string(),
            message: arguments
                .first()
                .map(Value::string_value)
                .unwrap_or_default(),
            code: None,
            class_name: "Error".to_string(),
        }))
    }

    fn construct_class(
        &self,
        class: ClassHandle,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        if class.name == "CeriBridgeError" || class.extends.as_deref() == Some("Error") {
            return Ok(Value::Error(Rc::new(ErrorData {
                name: class.name.clone(),
                message: arguments
                    .get(1)
                    .map(Value::string_value)
                    .unwrap_or_default(),
                code: arguments.first().map(Value::string_value),
                class_name: class.name.clone(),
            })));
        }
        Ok(Value::Object(Rc::new(RefCell::new(BTreeMap::new()))))
    }

    fn construct_promise(&mut self, arguments: Vec<Value>) -> Result<Value, ScriptError> {
        let promise = Rc::new(RefCell::new(PromiseData {
            outcome: PromiseOutcome::Pending,
            reactions: Vec::new(),
        }));
        if let Some(executor) = arguments.first() {
            if !matches!(executor, Value::Function(_)) {
                return Err(runtime_error("Promise executor must be a function"));
            }
            let resolve = Value::Builtin(Builtin::PromiseResolvePending(promise.clone()));
            let reject = Value::Builtin(Builtin::PromiseRejectPending(promise.clone()));
            self.call(executor.clone(), vec![resolve, reject])?;
        }
        Ok(Value::Promise(promise))
    }

    fn submit_form(&mut self, path: NodePath) -> Result<(), ScriptError> {
        let form = self
            .document
            .borrow()
            .element_clone(&path)
            .ok_or_else(|| runtime_error("form is no longer in the document"))?;
        let method = form
            .attr("method")
            .map(|method| method.to_ascii_lowercase())
            .unwrap_or_else(|| "get".to_string());
        if method != "get" {
            return Err(runtime_error(
                "only GET form submission is available in the packaged engine",
            ));
        }
        let Value::FormData(form_data) = self.construct_form_data(vec![Value::Element(
            attached_handle(path),
        )])? else {
            unreachable!("form data constructor returns form data");
        };
        let base = self
            .base_url
            .clone()
            .ok_or_else(|| runtime_error("form submission requires a document URL"))?;
        let action = form.attr("action").unwrap_or("");
        let mut url = base
            .resolve(action)
            .map_err(|error| runtime_error(error.to_string()))?;
        let encoded = serialize_form_urlencoded(&form_data.borrow().pairs);
        if !encoded.is_empty() {
            let query = if url.query().is_empty() {
                encoded
            } else {
                format!("{}&{encoded}", url.query())
            };
            url = url.with_query_fragment(query, url.fragment().to_string());
        }
        self.request_location_navigation(url.to_string(), false);
        Ok(())
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
            Builtin::DocumentQuerySelectorAll => {
                let selector = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let nodes = self
                    .document
                    .borrow()
                    .find_elements_by_selector(&selector)
                    .unwrap_or_default()
                    .into_iter()
                    .map(attached_handle)
                    .collect();
                Ok(Value::NodeList(Rc::new(nodes)))
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
            Builtin::StringSlice(string) => {
                let length = string.chars().count() as isize;
                let start = arguments
                    .first()
                    .map(Value::number)
                    .transpose()?
                    .unwrap_or(0.0) as isize;
                let end = arguments
                    .get(1)
                    .map(Value::number)
                    .transpose()?
                    .map(|value| value as isize)
                    .unwrap_or(length);
                let start = start.clamp(0, length) as usize;
                let end = end.clamp(0, length) as usize;
                let value = string
                    .chars()
                    .skip(start.min(end))
                    .take(end.saturating_sub(start))
                    .collect();
                Ok(Value::String(value))
            }
            Builtin::SetHas(set) => {
                let candidate = arguments.first().cloned().unwrap_or(Value::Undefined);
                Ok(Value::Bool(
                    set.borrow()
                        .values
                        .iter()
                        .any(|value| values_equal(value, &candidate)),
                ))
            }
            Builtin::PromiseReject => Ok(Value::Promise(Rc::new(RefCell::new(PromiseData {
                outcome: PromiseOutcome::Rejected(
                    arguments.first().cloned().unwrap_or(Value::Undefined),
                ),
                reactions: Vec::new(),
            })))),
            Builtin::PromiseResolve => Ok(Value::Promise(self.fulfilled_promise(
                arguments.first().cloned().unwrap_or(Value::Undefined),
            ))),
            Builtin::PromiseRejectPending(promise) => {
                let reason = arguments.first().cloned().unwrap_or(Value::Undefined);
                self.settle_promise(&promise, PromiseOutcome::Rejected(reason));
                Ok(Value::Undefined)
            }
            Builtin::PromiseResolvePending(promise) => {
                let value = arguments.first().cloned().unwrap_or(Value::Undefined);
                self.settle_promise(&promise, PromiseOutcome::Fulfilled(value));
                Ok(Value::Undefined)
            }
            Builtin::NumberParseInt => {
                let source = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let radix = arguments
                    .get(1)
                    .map(Value::number)
                    .transpose()?
                    .unwrap_or(10.0) as u32;
                let parsed = parse_integer(&source, radix).unwrap_or(f64::NAN);
                Ok(Value::Number(parsed))
            }
            Builtin::NumberIsFinite => Ok(Value::Bool(
                arguments
                    .first()
                    .is_some_and(|value| matches!(value, Value::Number(number) if number.is_finite())),
            )),
            Builtin::ArrayIsArray => Ok(Value::Bool(
                arguments.first().is_some_and(|value| matches!(value, Value::Array(_))),
            )),
            Builtin::ObjectFreeze => Ok(arguments.first().cloned().unwrap_or(Value::Undefined)),
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
            Builtin::ElementFocus(path) => {
                self.focus_path(path)?;
                Ok(Value::Undefined)
            }
            Builtin::ElementBlur(path) => {
                if self.focused_element.as_ref() == Some(&path) {
                    self.blur_focused()?;
                }
                Ok(Value::Undefined)
            }
            Builtin::ElementRequestSubmit(path) => {
                self.request_submit(path)?;
                Ok(Value::Undefined)
            }
            Builtin::CanvasGetContext(path) => {
                let kind = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                if !kind.eq_ignore_ascii_case("2d") {
                    return Ok(Value::Null);
                }
                let context = self
                    .canvas_contexts
                    .entry(path.clone())
                    .or_insert_with(|| {
                        Rc::new(RefCell::new(CanvasContextData {
                            path,
                            fill_style: Color::BLACK,
                        }))
                    })
                    .clone();
                Ok(Value::CanvasContext(context))
            }
            Builtin::CanvasFillRect(context) => {
                let x = canvas_number(&arguments, 0)?;
                let y = canvas_number(&arguments, 1)?;
                let width = canvas_number(&arguments, 2)?;
                let height = canvas_number(&arguments, 3)?;
                self.canvas_commands.push(CanvasCommand::FillRect {
                    path: context.borrow().path.clone(),
                    x,
                    y,
                    width,
                    height,
                    color: context.borrow().fill_style,
                });
                Ok(Value::Undefined)
            }
            Builtin::CanvasClearRect(context) => {
                let x = canvas_number(&arguments, 0)?;
                let y = canvas_number(&arguments, 1)?;
                let width = canvas_number(&arguments, 2)?;
                let height = canvas_number(&arguments, 3)?;
                self.canvas_commands.push(CanvasCommand::ClearRect {
                    path: context.borrow().path.clone(),
                    x,
                    y,
                    width,
                    height,
                });
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
            Builtin::StorageKey(storage) => {
                let index = arguments
                    .first()
                    .map(Value::number)
                    .transpose()?
                    .unwrap_or(-1.0);
                if !index.is_finite() || index < 0.0 || index.fract() != 0.0 {
                    return Ok(Value::Null);
                }
                Ok(storage
                    .borrow()
                    .values
                    .keys()
                    .nth(index as usize)
                    .cloned()
                    .map(Value::String)
                    .unwrap_or(Value::Null))
            }
            Builtin::StorageGetItem(storage) => {
                let key = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_else(|| "undefined".to_string());
                Ok(storage
                    .borrow()
                    .values
                    .get(&key)
                    .cloned()
                    .map(Value::String)
                    .unwrap_or(Value::Null))
            }
            Builtin::StorageSetItem(storage) => {
                let key = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_else(|| "undefined".to_string());
                let value = arguments
                    .get(1)
                    .map(Value::string_value)
                    .unwrap_or_else(|| "undefined".to_string());
                storage.borrow_mut().values.insert(key, value);
                Ok(Value::Undefined)
            }
            Builtin::StorageRemoveItem(storage) => {
                let key = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_else(|| "undefined".to_string());
                storage.borrow_mut().values.remove(&key);
                Ok(Value::Undefined)
            }
            Builtin::StorageClear(storage) => {
                storage.borrow_mut().values.clear();
                Ok(Value::Undefined)
            }
            Builtin::FormDataAppend(form_data) => {
                let name = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let value = arguments
                    .get(1)
                    .map(Value::string_value)
                    .unwrap_or_else(|| "undefined".to_string());
                form_data.borrow_mut().pairs.push((name, value));
                Ok(Value::Undefined)
            }
            Builtin::FormDataDelete(form_data) => {
                let name = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                form_data
                    .borrow_mut()
                    .pairs
                    .retain(|(entry_name, _)| entry_name != &name);
                Ok(Value::Undefined)
            }
            Builtin::FormDataGet(form_data) => {
                let name = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                Ok(form_data
                    .borrow()
                    .pairs
                    .iter()
                    .find(|(entry_name, _)| entry_name == &name)
                    .map(|(_, value)| Value::String(value.clone()))
                    .unwrap_or(Value::Null))
            }
            Builtin::FormDataGetAll(form_data) => {
                let name = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                Ok(Value::Array(Rc::new(RefCell::new(
                    form_data
                        .borrow()
                        .pairs
                        .iter()
                        .filter(|(entry_name, _)| entry_name == &name)
                        .map(|(_, value)| Value::String(value.clone()))
                        .collect(),
                ))))
            }
            Builtin::FormDataHas(form_data) => {
                let name = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                Ok(Value::Bool(
                    form_data
                        .borrow()
                        .pairs
                        .iter()
                        .any(|(entry_name, _)| entry_name == &name),
                ))
            }
            Builtin::FormDataSet(form_data) => {
                let name = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let value = arguments
                    .get(1)
                    .map(Value::string_value)
                    .unwrap_or_else(|| "undefined".to_string());
                let mut form_data = form_data.borrow_mut();
                if let Some(index) = form_data
                    .pairs
                    .iter()
                    .position(|(entry_name, _)| entry_name == &name)
                {
                    form_data.pairs[index].1 = value;
                    let mut kept = false;
                    form_data.pairs.retain(|(entry_name, _)| {
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
                    form_data.pairs.push((name, value));
                }
                Ok(Value::Undefined)
            }
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
            Builtin::CeriQuery => {
                let request = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let Some(info) = self.host_runtime_info.clone() else {
                    return Ok(Value::Promise(
                        self.rejected_promise("host bridge is unavailable"),
                    ));
                };
                if request != RUNTIME_INFO_REQUEST {
                    return Ok(Value::Promise(self.rejected_promise(
                        "host bridge rejected an unsupported request",
                    )));
                }
                let mut payload = BTreeMap::new();
                payload.insert("engine".to_string(), Value::String(info.engine));
                payload.insert("chromium".to_string(), Value::String(info.chromium));
                payload.insert("recovery".to_string(), Value::String(info.recovery));
                payload.insert("session".to_string(), Value::String(info.session));
                let response = serialize_json(&Value::Object(Rc::new(RefCell::new(payload))))?;
                Ok(Value::Promise(
                    self.fulfilled_promise(Value::String(response)),
                ))
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
            Builtin::ElementQuerySelectorAll(path) => {
                let selector = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let nodes = self
                    .document
                    .borrow()
                    .find_elements_by_selector_within(&path, &selector)
                    .unwrap_or_default()
                    .into_iter()
                    .map(attached_handle)
                    .collect();
                Ok(Value::NodeList(Rc::new(nodes)))
            }
            Builtin::ArrayMap(array) => {
                let callback = arguments.first().cloned().unwrap_or(Value::Undefined);
                if !matches!(callback, Value::Function(_)) {
                    return Err(runtime_error("Array.map callback must be a function"));
                }
                let values = array.borrow().clone();
                let mut mapped = Vec::with_capacity(values.len());
                for (index, value) in values.into_iter().enumerate() {
                    mapped.push(self.call(
                        callback.clone(),
                        vec![
                            value,
                            Value::Number(index as f64),
                            Value::Array(array.clone()),
                        ],
                    )?);
                }
                Ok(Value::Array(Rc::new(RefCell::new(mapped))))
            }
            Builtin::ArrayJoin(array) => {
                let separator = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_else(|| ",".to_string());
                let joined = array
                    .borrow()
                    .iter()
                    .map(|value| match value {
                        Value::Undefined | Value::Null => String::new(),
                        value => value.string_value(),
                    })
                    .collect::<Vec<_>>()
                    .join(&separator);
                Ok(Value::String(joined))
            }
            Builtin::MapGet(map) => {
                let key = arguments.first().cloned().unwrap_or(Value::Undefined);
                Ok(map
                    .borrow()
                    .entries
                    .iter()
                    .find(|(entry_key, _)| values_equal(entry_key, &key))
                    .map(|(_, value)| value.clone())
                    .unwrap_or(Value::Undefined))
            }
            Builtin::MapSet(map) => {
                let key = arguments.first().cloned().unwrap_or(Value::Undefined);
                let value = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                let mut map_data = map.borrow_mut();
                if let Some((_, entry_value)) = map_data
                    .entries
                    .iter_mut()
                    .find(|(entry_key, _)| values_equal(entry_key, &key))
                {
                    *entry_value = value;
                } else {
                    map_data.entries.push((key, value));
                }
                drop(map_data);
                Ok(Value::Map(map))
            }
            Builtin::MapKeys(map) => Ok(iterator_value(
                map.borrow()
                    .entries
                    .iter()
                    .map(|(key, _)| key.clone())
                    .collect(),
            )),
            Builtin::MapValues(map) => Ok(iterator_value(
                map.borrow()
                    .entries
                    .iter()
                    .map(|(_, value)| value.clone())
                    .collect(),
            )),
            Builtin::MapEntries(map) => Ok(iterator_value(
                map.borrow()
                    .entries
                    .iter()
                    .map(|(key, value)| {
                        Value::Array(Rc::new(RefCell::new(vec![key.clone(), value.clone()])))
                    })
                    .collect(),
            )),
            Builtin::IteratorNext(iterator) => {
                let mut iterator = iterator.borrow_mut();
                let mut result = BTreeMap::new();
                if let Some(value) = iterator.values.get(iterator.index).cloned() {
                    iterator.index += 1;
                    result.insert("value".to_string(), value);
                    result.insert("done".to_string(), Value::Bool(false));
                } else {
                    result.insert("value".to_string(), Value::Undefined);
                    result.insert("done".to_string(), Value::Bool(true));
                }
                Ok(Value::Object(Rc::new(RefCell::new(result))))
            }
            Builtin::IteratorJoin(iterator) => {
                let separator = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_else(|| ",".to_string());
                let iterator = iterator.borrow();
                let joined = iterator.values[iterator.index..]
                    .iter()
                    .map(|value| match value {
                        Value::Undefined | Value::Null => String::new(),
                        value => value.string_value(),
                    })
                    .collect::<Vec<_>>()
                    .join(&separator);
                Ok(Value::String(joined))
            }
            Builtin::SetValues(set) => Ok(iterator_value(set.borrow().values.clone())),
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
        self.dispatch_event_with_details(path, event, "", "")
    }

    fn dispatch_event_with_details(
        &mut self,
        path: NodePath,
        event: &str,
        key: &str,
        data: &str,
    ) -> Result<EventDispatch, ScriptError> {
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
                key: key.to_string(),
                data: data.to_string(),
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
        self.rejected_promise_value(Value::String(reason.into()))
    }

    fn rejected_promise_value(&self, reason: Value) -> PromiseHandle {
        Rc::new(RefCell::new(PromiseData {
            outcome: PromiseOutcome::Rejected(reason),
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
        self.get_raw(name).map(|value| value.resolve()).or_else(|| {
            self.parent
                .as_ref()
                .and_then(|parent| parent.borrow().get(name))
        })
    }

    fn get_raw(&self, name: &str) -> Option<Value> {
        self.values.get(name).cloned().or_else(|| {
            self.parent
                .as_ref()
                .and_then(|parent| parent.borrow().get_raw(name))
        })
    }

    fn assign(&mut self, name: &str, value: Value) -> Result<bool, ScriptError> {
        if let Some(existing) = self.values.get(name).cloned() {
            match existing {
                Value::Binding(binding, true) => *binding.borrow_mut() = value,
                Value::Binding(_, false) => {
                    return Err(runtime_error(format!(
                        "cannot assign to imported binding {name}"
                    )))
                }
                _ => {
                    self.values.insert(name.to_string(), value);
                }
            }
            Ok(true)
        } else if let Some(parent) = &self.parent {
            parent.borrow_mut().assign(name, value)
        } else {
            Ok(false)
        }
    }
}

impl ModuleNamespace {
    fn object_value(&self) -> Value {
        Value::Object(Rc::new(RefCell::new(self.values.clone())))
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
    fn resolve(&self) -> Value {
        match self {
            Self::Binding(binding, _) => binding.borrow().resolve(),
            _ => self.clone(),
        }
    }

    fn truthy(&self) -> bool {
        let value = self.resolve();
        match &value {
            Self::Undefined | Self::Null => false,
            Self::Bool(value) => *value,
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::String(value) => !value.is_empty(),
            _ => true,
        }
    }

    fn number(&self) -> Result<f64, ScriptError> {
        let value = self.resolve();
        match &value {
            Self::Number(value) => Ok(*value),
            Self::String(value) => value
                .parse()
                .map_err(|_| runtime_error("value is not numeric")),
            Self::Bool(value) => Ok(f64::from(*value as u8)),
            _ => Err(runtime_error("value is not numeric")),
        }
    }

    fn string_value(&self) -> String {
        let value = self.resolve();
        match &value {
            Self::Undefined => "undefined".to_string(),
            Self::Null => "null".to_string(),
            Self::Bool(value) => value.to_string(),
            Self::Number(value) if value.fract() == 0.0 => format!("{value:.0}"),
            Self::Number(value) => value.to_string(),
            Self::String(value) => value.clone(),
            Self::Symbol(value) => format!("Symbol({value})"),
            Self::Binding(_, _) => unreachable!("binding values resolve before formatting"),
            Self::Object(_) => "[object Object]".to_string(),
            Self::Array(_) => "[array]".to_string(),
            Self::NodeList(_) => "[object NodeList]".to_string(),
            Self::Map(_) => "[object Map]".to_string(),
            Self::Set(_) => "[object Set]".to_string(),
            Self::Iterator(_) => "[object Iterator]".to_string(),
            Self::Element(_) => "[element]".to_string(),
            Self::Dataset(_) => "[object DOMStringMap]".to_string(),
            Self::Error(error) => error.message.clone(),
            Self::Style(_) => "[style]".to_string(),
            Self::ClassList(_) => "[classList]".to_string(),
            Self::Event(_) => "[event]".to_string(),
            Self::Response(_) => "[response]".to_string(),
            Self::Promise(_) => "[promise]".to_string(),
            Self::Document => "[document]".to_string(),
            Self::Window => "[window]".to_string(),
            Self::Location(value) => value.borrow().to_string(),
            Self::ModuleImport(_) => "[object Module]".to_string(),
            Self::History(_) => "[object History]".to_string(),
            Self::Url(value) => value.borrow().url.to_string(),
            Self::UrlSearchParams(value) => serialize_form_urlencoded(&value.borrow().pairs),
            Self::Storage(_) => "[object Storage]".to_string(),
            Self::FormData(_) => "[object FormData]".to_string(),
            Self::CanvasContext(_) => "[object CanvasRenderingContext2D]".to_string(),
            Self::Console => "[console]".to_string(),
            Self::Json => "[object JSON]".to_string(),
            Self::UrlConstructor | Self::UrlSearchParamsConstructor => {
                "[function URL]".to_string()
            }
            Self::FormDataConstructor => "[function FormData]".to_string(),
            Self::MapConstructor => "[function Map]".to_string(),
            Self::SetConstructor => "[function Set]".to_string(),
            Self::ErrorConstructor => "[function Error]".to_string(),
            Self::PromiseConstructor => "[function Promise]".to_string(),
            Self::NumberNamespace => "[function Number]".to_string(),
            Self::ArrayNamespace => "[function Array]".to_string(),
            Self::ObjectNamespace => "[function Object]".to_string(),
            Self::StringConstructor => "[function String]".to_string(),
            Self::SymbolNamespace => "[function Symbol]".to_string(),
            Self::Class(class) => format!("[class {}]", class.name),
            Self::Function(_) => "[function]".to_string(),
            Self::Builtin(_) => "[function]".to_string(),
        }
    }

    fn display_string(&self) -> String {
        self.string_value()
    }

    fn property_key(&self) -> String {
        match self.resolve() {
            Self::Symbol(value) => value.clone(),
            _ => self.string_value(),
        }
    }
}

fn serialize_json(value: &Value) -> Result<String, ScriptError> {
    let value = value.resolve();
    match &value {
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

fn canvas_number(arguments: &[Value], index: usize) -> Result<f32, ScriptError> {
    let value = arguments
        .get(index)
        .map(Value::number)
        .transpose()?
        .unwrap_or(0.0);
    if !value.is_finite() {
        return Err(runtime_error("canvas coordinates must be finite"));
    }
    Ok(value as f32)
}

fn color_css(color: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", color.red, color.green, color.blue)
}

fn dataset_attribute_name(property: &str) -> String {
    let mut name = String::from("data-");
    for character in property.chars() {
        if character.is_ascii_uppercase() {
            name.push('-');
            name.push(character.to_ascii_lowercase());
        } else {
            name.push(character);
        }
    }
    name
}

fn parse_integer(source: &str, radix: u32) -> Option<f64> {
    if !(2..=36).contains(&radix) {
        return None;
    }
    let source = source.trim();
    let (negative, digits) = source
        .strip_prefix('-')
        .map_or((false, source), |digits| (true, digits));
    let digits = digits.strip_prefix('+').unwrap_or(digits);
    if digits.is_empty() {
        return None;
    }
    let value = i128::from_str_radix(digits, radix).ok()? as f64;
    Some(if negative { -value } else { value })
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
        BinaryOperator::InstanceOf => Ok(Value::Bool(match (left, right) {
            (Value::Error(_), Value::ErrorConstructor) => true,
            (Value::Error(error), Value::Class(class)) => {
                error.class_name == class.name
                    || (class.name == "Error" && error.class_name == "Error")
            }
            _ => false,
        })),
        BinaryOperator::And | BinaryOperator::Or => unreachable!("logical operators short-circuit"),
    }
}

fn value_typeof(value: &Value) -> &'static str {
    let value = value.resolve();
    match &value {
        Value::Undefined => "undefined",
        Value::Null
        | Value::Object(_)
        | Value::Array(_)
        | Value::NodeList(_)
        | Value::Map(_)
        | Value::Set(_)
        | Value::Iterator(_)
        | Value::Element(_)
        | Value::Dataset(_)
        | Value::Style(_)
        | Value::ClassList(_)
        | Value::Event(_)
        | Value::Response(_)
        | Value::Promise(_)
        | Value::Document
        | Value::Window
        | Value::Location(_)
        | Value::ModuleImport(_)
        | Value::History(_)
        | Value::Url(_)
        | Value::UrlSearchParams(_)
        | Value::Storage(_)
        | Value::FormData(_)
        | Value::CanvasContext(_)
        | Value::Console
        | Value::Json
        | Value::Error(_) => "object",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Symbol(_) => "symbol",
        Value::Binding(_, _) => unreachable!("binding values resolve before typeof"),
        Value::Function(_)
        | Value::Builtin(_)
        | Value::UrlConstructor
        | Value::UrlSearchParamsConstructor
        | Value::FormDataConstructor
        | Value::MapConstructor
        | Value::SetConstructor
        | Value::ErrorConstructor
        | Value::PromiseConstructor
        | Value::NumberNamespace
        | Value::ArrayNamespace
        | Value::ObjectNamespace
        | Value::StringConstructor
        | Value::SymbolNamespace
        | Value::Class(_) => "function",
    }
}

fn error_handle(value: &Value) -> ErrorHandle {
    match value.resolve() {
        Value::Error(error) => error.clone(),
        _ => Rc::new(ErrorData {
            name: "Error".to_string(),
            message: value.string_value(),
            code: None,
            class_name: "Error".to_string(),
        }),
    }
}

fn values_equal(left: &Value, right: &Value) -> bool {
    let left = left.resolve();
    let right = right.resolve();
    match (&left, &right) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Bool(left), Value::Bool(right)) => left == right,
        (Value::Number(left), Value::Number(right)) => left == right,
        (Value::String(left), Value::String(right)) => left == right,
        (Value::Symbol(left), Value::Symbol(right)) => left == right,
        (Value::Binding(_, _), _) | (_, Value::Binding(_, _)) => {
            unreachable!("binding values resolve before comparison")
        }
        (Value::Element(left), Value::Element(right))
        | (Value::Style(left), Value::Style(right)) => left == right,
        (Value::Url(left), Value::Url(right)) => Rc::ptr_eq(left, right),
        (Value::UrlSearchParams(left), Value::UrlSearchParams(right)) => Rc::ptr_eq(left, right),
        (Value::Storage(left), Value::Storage(right)) => Rc::ptr_eq(left, right),
        (Value::FormData(left), Value::FormData(right)) => Rc::ptr_eq(left, right),
        (Value::CanvasContext(left), Value::CanvasContext(right)) => Rc::ptr_eq(left, right),
        _ => false,
    }
}

fn iterator_value(values: Vec<Value>) -> Value {
    Value::Iterator(Rc::new(RefCell::new(IteratorData { values, index: 0 })))
}

const MAX_USER_ITERATOR_VALUES: usize = 10_000;

impl ScriptRuntime {
    fn spread_values(&mut self, value: Value) -> Result<Vec<Value>, ScriptError> {
        self.iterable_values(value)
    }

    fn iterable_values(&mut self, value: Value) -> Result<Vec<Value>, ScriptError> {
        match value.resolve() {
            Value::Array(values) => Ok(values.borrow().clone()),
            Value::NodeList(nodes) => Ok(nodes.iter().cloned().map(Value::Element).collect()),
            Value::String(string) => Ok(string
                .chars()
                .map(|character| Value::String(character.to_string()))
                .collect()),
            Value::Iterator(iterator) => {
                let mut iterator = iterator.borrow_mut();
                let values = iterator.values[iterator.index..].to_vec();
                iterator.index = iterator.values.len();
                Ok(values)
            }
            Value::Set(set) => Ok(set.borrow().values.clone()),
            Value::Map(map) => Ok(map
                .borrow()
                .entries
                .iter()
                .map(|(key, value)| {
                    Value::Array(Rc::new(RefCell::new(vec![key.clone(), value.clone()])))
                })
                .collect()),
            Value::Object(object) => {
                let iterator = object.borrow().get("@@iterator").cloned();
                let Some(iterator) = iterator else {
                    return Err(runtime_error("value is not iterable"));
                };
                let iterator = self.call(iterator, Vec::new())?;
                let mut values = Vec::new();
                loop {
                    if values.len() == MAX_USER_ITERATOR_VALUES {
                        return Err(runtime_error(
                            "user-defined iterator exceeded the deterministic limit",
                        ));
                    }
                    let next = self.member(iterator.clone(), "next")?;
                    let record = self.call(next, Vec::new())?;
                    if self.member(record.clone(), "done")?.truthy() {
                        return Ok(values);
                    }
                    values.push(self.member(record, "value")?);
                }
            }
            _ => Err(runtime_error("value is not iterable")),
        }
    }
}

pub(crate) fn runtime_error(message: impl Into<String>) -> ScriptError {
    ScriptError {
        offset: 0,
        message: message.into(),
    }
}

fn parse_dynamic_import_type(
    attributes: Option<&Value>,
) -> Result<Option<DynamicImportType>, ScriptError> {
    let Some(attributes) = attributes else {
        return Ok(None);
    };
    let Value::Object(attributes) = attributes else {
        return Err(runtime_error("dynamic import options must be an object"));
    };
    let attributes = attributes.borrow();
    if attributes.keys().any(|key| key != "with") {
        return Err(runtime_error("dynamic import options contain an unsupported key"));
    }
    let Some(with) = attributes.get("with") else {
        return Err(runtime_error("dynamic import options require a with object"));
    };
    let Value::Object(with) = with else {
        return Err(runtime_error("dynamic import with must be an object"));
    };
    let with = with.borrow();
    if with.keys().any(|key| key != "type") {
        return Err(runtime_error("dynamic import with contains an unsupported key"));
    }
    let Some(module_type) = with.get("type") else {
        return Err(runtime_error("dynamic import with requires a type"));
    };
    let Value::String(module_type) = module_type else {
        return Err(runtime_error("dynamic import type must be a string"));
    };
    if module_type != "json" {
        return Err(runtime_error(format!(
            "unsupported dynamic import type {module_type}"
        )));
    }
    Ok(Some(DynamicImportType::Json))
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

    fn parse_module(&mut self) -> Result<ModuleProgram, ScriptError> {
        let mut imports = Vec::new();
        let mut star_reexports = Vec::new();
        let mut statements = Vec::new();
        let mut exports = Vec::new();
        while !self.is(TokenKind::Eof) {
            if self.is_identifier("import") && !self.is_next(TokenKind::Dot) {
                imports.push(self.parse_import_declaration()?);
            } else if self.is_identifier("export") {
                self.advance();
                self.parse_export_declaration(
                    &mut imports,
                    &mut star_reexports,
                    &mut statements,
                    &mut exports,
                )?;
            } else {
                statements.push(self.parse_statement()?);
            }
        }
        Ok(ModuleProgram {
            imports,
            star_reexports,
            statements,
            exports,
        })
    }

    fn parse_import_declaration(&mut self) -> Result<ModuleImport, ScriptError> {
        self.expect_identifier_value("import")?;
        if let TokenKind::String(source) = self.peek().kind.clone() {
            self.advance();
            self.take(TokenKind::Semicolon);
            return Ok(ModuleImport {
                source,
                bindings: Vec::new(),
            });
        }
        let mut bindings = Vec::new();
        if let TokenKind::Identifier(local) = self.peek().kind.clone() {
            self.advance();
            bindings.push(ModuleImportBinding::Named {
                imported: "default".to_string(),
                local,
            });
            self.take(TokenKind::Comma);
        }
        if self.take(TokenKind::Star) {
            self.expect_identifier_value("as")?;
            let local = self.expect_identifier("namespace import name")?;
            bindings.push(ModuleImportBinding::Namespace { local });
        } else if self.take(TokenKind::LBrace) {
            if !self.is(TokenKind::RBrace) {
                loop {
                    let imported = self.expect_identifier("import name")?;
                    let local = if self.is_identifier("as") {
                        self.advance();
                        self.expect_identifier("local import name")?
                    } else {
                        imported.clone()
                    };
                    bindings.push(ModuleImportBinding::Named { imported, local });
                    if !self.take(TokenKind::Comma) {
                        break;
                    }
                    if self.is(TokenKind::RBrace) {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RBrace, "named imports")?;
        }
        self.expect_identifier_value("from")?;
        let source = self.expect_string("module specifier")?;
        self.take(TokenKind::Semicolon);
        if bindings.is_empty() {
            return Err(self.error(
                "module import has no bindings".to_string(),
                self.peek().offset,
            ));
        }
        Ok(ModuleImport { source, bindings })
    }

    fn parse_export_declaration(
        &mut self,
        imports: &mut Vec<ModuleImport>,
        star_reexports: &mut Vec<String>,
        statements: &mut Vec<Stmt>,
        exports: &mut Vec<ModuleExport>,
    ) -> Result<(), ScriptError> {
        if self.take(TokenKind::Star) {
            self.expect_identifier_value("from")?;
            star_reexports.push(self.expect_string("export-star module specifier")?);
            self.take(TokenKind::Semicolon);
            return Ok(());
        }
        if self.is_identifier("default") {
            self.advance();
            if exports.iter().any(|export| export.exported == "default") {
                return Err(self.error(
                    "module has more than one default export".to_string(),
                    self.peek().offset,
                ));
            }
            let local = "__ceri_default_export".to_string();
            let initializer = self.parse_expression()?;
            statements.push(Stmt::Let {
                pattern: Pattern::Identifier(local.clone()),
                initializer: Some(initializer),
            });
            exports.push(ModuleExport {
                local,
                exported: "default".to_string(),
            });
            self.take(TokenKind::Semicolon);
            return Ok(());
        }
        if self.is(TokenKind::Let) || self.is(TokenKind::Const) {
            let statement = self.parse_statement()?;
            let Stmt::Let {
                pattern: Pattern::Identifier(name),
                ..
            } = &statement
            else {
                return Err(self.error("expected exported binding".to_string(), self.peek().offset));
            };
            exports.push(ModuleExport {
                local: name.clone(),
                exported: name.clone(),
            });
            statements.push(statement);
            return Ok(());
        }
        if self.is(TokenKind::Function) && self.is_next_identifier() {
            let statement = self.parse_statement()?;
            let Stmt::Let {
                pattern: Pattern::Identifier(name),
                ..
            } = &statement
            else {
                return Err(self.error("expected exported function".to_string(), self.peek().offset));
            };
            exports.push(ModuleExport {
                local: name.clone(),
                exported: name.clone(),
            });
            statements.push(statement);
            return Ok(());
        }
        if self.is(TokenKind::Class) {
            let statement = self.parse_statement()?;
            let Stmt::Class { name, .. } = &statement else {
                return Err(self.error("expected exported class".to_string(), self.peek().offset));
            };
            exports.push(ModuleExport {
                local: name.clone(),
                exported: name.clone(),
            });
            statements.push(statement);
            return Ok(());
        }
        if self.take(TokenKind::LBrace) {
            let mut bindings = Vec::new();
            if !self.is(TokenKind::RBrace) {
                loop {
                    let local = self.expect_identifier("export name")?;
                    let exported = if self.is_identifier("as") {
                        self.advance();
                        self.expect_identifier("export alias")?
                    } else {
                        local.clone()
                    };
                    bindings.push((local, exported));
                    if !self.take(TokenKind::Comma) {
                        break;
                    }
                    if self.is(TokenKind::RBrace) {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RBrace, "export list")?;
            if self.is_identifier("from") {
                self.advance();
                let source = self.expect_string("re-export module specifier")?;
                for (index, (imported, exported)) in bindings.into_iter().enumerate() {
                    let local = format!("__ceri_reexport_{}_{index}", imports.len());
                    imports.push(ModuleImport {
                        source: source.clone(),
                        bindings: vec![ModuleImportBinding::Named {
                            imported,
                            local: local.clone(),
                        }],
                    });
                    exports.push(ModuleExport { local, exported });
                }
            } else {
                exports.extend(
                    bindings
                        .into_iter()
                        .map(|(local, exported)| ModuleExport { local, exported }),
                );
            }
            self.take(TokenKind::Semicolon);
            return Ok(());
        }
        Err(self.error(
            "module export form is outside the supported subset".to_string(),
            self.peek().offset,
        ))
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
            let pattern = self.parse_binding_pattern_without_default("variable")?;
            let initializer = self
                .take(TokenKind::Assign)
                .then(|| self.parse_expression())
                .transpose()?;
            self.take(TokenKind::Semicolon);
            return Ok(Stmt::Let {
                pattern,
                initializer,
            });
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
        if self.take(TokenKind::For) {
            self.expect(TokenKind::LParen, "for statement")?;
            if !self.take(TokenKind::Let) && !self.take(TokenKind::Const) {
                return Err(self.error(
                    "for-of binding must use let or const".to_string(),
                    self.peek().offset,
                ));
            }
            let pattern = self.parse_binding_pattern_without_default("for-of binding")?;
            self.expect_identifier_value("of")?;
            let iterable = self.parse_expression()?;
            self.expect(TokenKind::RParen, "for statement")?;
            let body = Box::new(self.parse_statement()?);
            return Ok(Stmt::ForOf {
                pattern,
                iterable,
                body,
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
                pattern: Pattern::Identifier(name),
                initializer: Some(function),
            });
        }
        if self.take(TokenKind::Async) {
            self.expect(TokenKind::Function, "async function")?;
            let name = self.expect_identifier("async function name")?;
            let function = self.parse_function_after_name_with_async(true)?;
            self.take(TokenKind::Semicolon);
            return Ok(Stmt::Let {
                pattern: Pattern::Identifier(name),
                initializer: Some(function),
            });
        }
        if self.take(TokenKind::Try) {
            let body = Box::new(self.parse_statement()?);
            self.expect(TokenKind::Catch, "catch clause")?;
            let catch_name = if self.take(TokenKind::LParen) {
                let name = self.expect_identifier("catch binding")?;
                self.expect(TokenKind::RParen, "catch clause")?;
                Some(name)
            } else {
                None
            };
            let handler = Box::new(self.parse_statement()?);
            return Ok(Stmt::TryCatch {
                body,
                catch_name,
                handler,
            });
        }
        if self.take(TokenKind::Throw) {
            let expression = self.parse_expression()?;
            self.take(TokenKind::Semicolon);
            return Ok(Stmt::Throw(expression));
        }
        if self.take(TokenKind::Class) {
            return self.parse_class_declaration();
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

    fn parse_class_declaration(&mut self) -> Result<Stmt, ScriptError> {
        let name = self.expect_identifier("class name")?;
        let extends = if self.is_identifier("extends") {
            self.advance();
            Some(self.expect_identifier("base class name")?)
        } else {
            None
        };
        self.expect(TokenKind::LBrace, "class body")?;
        while !self.is(TokenKind::RBrace) && !self.is(TokenKind::Eof) {
            let method = self.expect_identifier("class method name")?;
            let _function = self.parse_function_after_name()?;
            if method != "constructor" {
                return Err(self.error(
                    "only class constructors are supported".to_string(),
                    self.peek().offset,
                ));
            }
        }
        self.expect(TokenKind::RBrace, "class body")?;
        Ok(Stmt::Class { name, extends })
    }

    fn parse_expression(&mut self) -> Result<Expr, ScriptError> {
        self.parse_assignment()
    }

    fn parse_assignment(&mut self) -> Result<Expr, ScriptError> {
        let left = self.parse_or()?;
        if self.take(TokenKind::Question) {
            let consequent = self.parse_expression()?;
            self.expect(TokenKind::Colon, "conditional expression")?;
            let alternate = self.parse_assignment()?;
            return Ok(Expr::Conditional {
                condition: Box::new(left),
                consequent: Box::new(consequent),
                alternate: Box::new(alternate),
            });
        }
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
                TokenKind::InstanceOf,
            ],
            &[
                BinaryOperator::Less,
                BinaryOperator::LessEqual,
                BinaryOperator::Greater,
                BinaryOperator::GreaterEqual,
                BinaryOperator::InstanceOf,
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
        } else if self.take(TokenKind::TypeOf) {
            Some(UnaryOperator::TypeOf)
        } else if self.take(TokenKind::Await) {
            Some(UnaryOperator::Await)
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
                let property = self.expect_property_name()?;
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
                        if self.is(TokenKind::RParen) {
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
            TokenKind::Template(value) => Ok(Expr::Template(self.parse_template_parts(&value)?)),
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
                    Ok(self.parse_arrow_body(vec![Pattern::Identifier(name)])?)
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

    fn parse_template_parts(&self, source: &str) -> Result<Vec<TemplatePart>, ScriptError> {
        let mut parts = Vec::new();
        let mut cursor = 0;
        let mut text_start = 0;
        while let Some(relative_start) = source[cursor..].find("${") {
            let start = cursor + relative_start;
            if start > text_start {
                parts.push(TemplatePart::Text(source[text_start..start].to_string()));
            }
            let expression_start = start + 2;
            let end = find_template_expression_end(source, expression_start).ok_or_else(|| {
                self.error(
                    "unterminated template interpolation".to_string(),
                    start,
                )
            })?;
            let mut parser = Parser::new(&source[expression_start..end])?;
            let expression = parser.parse_expression()?;
            if !parser.is(TokenKind::Eof) {
                return Err(self.error(
                    "template interpolation contains trailing tokens".to_string(),
                    expression_start + parser.peek().offset,
                ));
            }
            parts.push(TemplatePart::Expression(Box::new(expression)));
            cursor = end + 1;
            text_start = cursor;
        }
        if text_start < source.len() {
            parts.push(TemplatePart::Text(source[text_start..].to_string()));
        }
        Ok(parts)
    }

    fn parse_object_literal(&mut self) -> Result<Expr, ScriptError> {
        let mut properties = Vec::new();
        if !self.is(TokenKind::RBrace) {
            loop {
                let key = if self.take(TokenKind::LBracket) {
                    let expression = self.parse_expression()?;
                    self.expect(TokenKind::RBracket, "computed object property name")?;
                    ObjectLiteralKey::Computed(expression)
                } else {
                    let token = self.advance();
                    let key = match token.kind {
                        TokenKind::Identifier(key) | TokenKind::String(key) => key,
                        _ => {
                            return Err(self.error(
                                "expected object property name".to_string(),
                                token.offset,
                            ));
                        }
                    };
                    ObjectLiteralKey::Static(key)
                };
                let value = if self.take(TokenKind::Colon) {
                    self.parse_expression()?
                } else if self.is(TokenKind::LParen) {
                    self.parse_function_after_name_with_async(false)?
                } else {
                    return Err(self.error(
                        "object property needs a value or method body".to_string(),
                        self.peek().offset,
                    ));
                };
                properties.push((key, value));
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
                let expression = if self.take(TokenKind::Ellipsis) {
                    Expr::Spread(Box::new(self.parse_expression()?))
                } else {
                    self.parse_expression()?
                };
                values.push(expression);
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

    fn parse_binding_pattern(&mut self, context: &str) -> Result<Pattern, ScriptError> {
        self.parse_binding_pattern_with_default(context, true)
    }

    fn parse_binding_pattern_without_default(&mut self, context: &str) -> Result<Pattern, ScriptError> {
        self.parse_binding_pattern_with_default(context, false)
    }

    fn parse_binding_pattern_with_default(
        &mut self,
        context: &str,
        allow_default: bool,
    ) -> Result<Pattern, ScriptError> {
        let pattern = match self.peek().kind.clone() {
            TokenKind::Identifier(name) => {
                self.advance();
                Pattern::Identifier(name)
            }
            TokenKind::LBrace => {
                self.advance();
                let mut properties = Vec::new();
                let mut rest = None;
                while !self.is(TokenKind::RBrace) && !self.is(TokenKind::Eof) {
                    if self.take(TokenKind::Ellipsis) {
                        rest = Some(Box::new(
                            self.parse_binding_pattern_with_default("object rest", false)?,
                        ));
                        self.take(TokenKind::Comma);
                        break;
                    }
                    let key = if self.take(TokenKind::LBracket) {
                        let expression = self.parse_expression()?;
                        self.expect(TokenKind::RBracket, "computed object binding name")?;
                        ObjectPatternKey::Computed(expression)
                    } else {
                        let token = self.advance();
                        let key = match token.kind {
                            TokenKind::Identifier(key) | TokenKind::String(key) => key,
                            _ => {
                                return Err(self.error(
                                    "expected object binding name".to_string(),
                                    token.offset,
                                ));
                            }
                        };
                        ObjectPatternKey::Static(key)
                    };
                    let nested = if self.take(TokenKind::Colon) {
                        self.parse_binding_pattern("object binding")?
                    } else {
                        let ObjectPatternKey::Static(key) = &key else {
                            return Err(self.error(
                                "computed object binding needs a target".to_string(),
                                self.peek().offset,
                            ));
                        };
                        self.parse_pattern_default(Pattern::Identifier(key.clone()))?
                    };
                    properties.push((key, nested));
                    if !self.take(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::RBrace, context)?;
                Pattern::Object { properties, rest }
            }
            TokenKind::LBracket => {
                self.advance();
                let mut items = Vec::new();
                let mut rest = None;
                while !self.is(TokenKind::RBracket) && !self.is(TokenKind::Eof) {
                    if self.take(TokenKind::Ellipsis) {
                        rest = Some(Box::new(
                            self.parse_binding_pattern_with_default("array rest", false)?,
                        ));
                        self.take(TokenKind::Comma);
                        break;
                    }
                    if self.take(TokenKind::Comma) {
                        items.push(None);
                        continue;
                    }
                    items.push(Some(self.parse_binding_pattern("array binding")?));
                    if !self.take(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::RBracket, context)?;
                Pattern::Array { items, rest }
            }
            _ => return Err(self.error(format!("expected {context}"), self.peek().offset)),
        };
        if allow_default {
            self.parse_pattern_default(pattern)
        } else {
            Ok(pattern)
        }
    }

    fn parse_pattern_default(&mut self, pattern: Pattern) -> Result<Pattern, ScriptError> {
        if self.take(TokenKind::Assign) {
            let default = self.parse_assignment()?;
            Ok(Pattern::Default {
                pattern: Box::new(pattern),
                default: Box::new(default),
            })
        } else {
            Ok(pattern)
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
                params.push(self.parse_binding_pattern("arrow parameter")?);
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

    fn parse_arrow_body(&mut self, params: Vec<Pattern>) -> Result<Expr, ScriptError> {
        let body = if self.take(TokenKind::LBrace) {
            self.parse_block_contents()?
        } else {
            vec![Stmt::Return(Some(self.parse_expression()?))]
        };
        Ok(Expr::Function {
            params,
            body,
            is_async: false,
        })
    }

    fn parse_function_expression(&mut self) -> Result<Expr, ScriptError> {
        if matches!(self.peek().kind, TokenKind::Identifier(_)) {
            self.advance();
        }
        let function = self.parse_function_after_name()?;
        let Expr::Function {
            params,
            body,
            is_async,
        } = function
        else {
            unreachable!("function parser returns a function expression");
        };
        Ok(Expr::Function {
            params,
            body,
            is_async,
        })
    }

    fn parse_function_after_name(&mut self) -> Result<Expr, ScriptError> {
        self.parse_function_after_name_with_async(false)
    }

    fn parse_function_after_name_with_async(
        &mut self,
        is_async: bool,
    ) -> Result<Expr, ScriptError> {
        self.expect(TokenKind::LParen, "function parameters")?;
        let mut params = Vec::new();
        if !self.is(TokenKind::RParen) {
            loop {
                params.push(self.parse_binding_pattern("function parameter")?);
                if !self.take(TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(TokenKind::RParen, "function parameters")?;
        self.expect(TokenKind::LBrace, "function body")?;
        let body = self.parse_block_contents()?;
        Ok(Expr::Function {
            params,
            body,
            is_async,
        })
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
                if self.is(TokenKind::RParen) {
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

    fn is_identifier(&self, expected: &str) -> bool {
        matches!(&self.peek().kind, TokenKind::Identifier(name) if name == expected)
    }

    fn is_next(&self, expected: TokenKind) -> bool {
        self.tokens
            .get(self.index + 1)
            .is_some_and(|token| token_matches(&token.kind, &expected))
    }

    fn expect_identifier_value(&mut self, expected: &str) -> Result<(), ScriptError> {
        if self.is_identifier(expected) {
            self.advance();
            Ok(())
        } else {
            Err(self.error(
                format!("expected {expected}"),
                self.peek().offset,
            ))
        }
    }

    fn expect_string(&mut self, context: &str) -> Result<String, ScriptError> {
        let token = self.advance();
        match token.kind {
            TokenKind::String(value) => Ok(value),
            _ => Err(self.error(format!("expected {context}"), token.offset)),
        }
    }

    fn expect_identifier(&mut self, context: &str) -> Result<String, ScriptError> {
        let token = self.advance();
        match token.kind {
            TokenKind::Identifier(name) => Ok(name),
            _ => Err(self.error(format!("expected {context}"), token.offset)),
        }
    }

    fn expect_property_name(&mut self) -> Result<String, ScriptError> {
        let token = self.advance();
        match token.kind {
            TokenKind::Identifier(name) => Ok(name),
            TokenKind::Catch => Ok("catch".to_string()),
            _ => Err(self.error("expected property name".to_string(), token.offset)),
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
    Template(String),
    True,
    False,
    Null,
    Let,
    Const,
    If,
    Else,
    Return,
    For,
    Try,
    Catch,
    Throw,
    Class,
    Async,
    Await,
    TypeOf,
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
    InstanceOf,
    Question,
    Ellipsis,
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
                "for" => TokenKind::For,
                "try" => TokenKind::Try,
                "catch" => TokenKind::Catch,
                "throw" => TokenKind::Throw,
                "class" => TokenKind::Class,
                "async" => TokenKind::Async,
                "await" => TokenKind::Await,
                "typeof" => TokenKind::TypeOf,
                "instanceof" => TokenKind::InstanceOf,
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
        if byte == b'`' {
            let start = cursor;
            cursor += 1;
            let mut value = String::new();
            let mut escaped = false;
            while cursor < source.len() {
                let character = source[cursor..]
                    .chars()
                    .next()
                    .expect("cursor stays on a character boundary");
                cursor += character.len_utf8();
                if escaped {
                    value.push(match character {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        other => other,
                    });
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '`' {
                    tokens.push(Token {
                        kind: TokenKind::Template(value),
                        offset: start,
                    });
                    break;
                } else {
                    value.push(character);
                }
                if cursor == source.len() {
                    return Err(ScriptError {
                        offset: start,
                        message: "unterminated template literal".to_string(),
                    });
                }
            }
            if !matches!(tokens.last().map(|token| &token.kind), Some(TokenKind::Template(_))) {
                return Err(ScriptError {
                    offset: start,
                    message: "unterminated template literal".to_string(),
                });
            }
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
            [b'.', b'.', b'.', ..] => (TokenKind::Ellipsis, 3),
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
            [b'?', ..] => (TokenKind::Question, 1),
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

fn find_template_expression_end(source: &str, start: usize) -> Option<usize> {
    let mut depth = 1;
    let mut quote = None;
    let mut escaped = false;
    for (offset, character) in source[start..].char_indices() {
        if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == active_quote {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(start + offset);
                }
            }
            _ => {}
        }
    }

    None
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

    struct StaticModuleLoader {
        modules: BTreeMap<String, String>,
    }

    impl ModuleSourceLoader for StaticModuleLoader {
        fn load_module(
            &mut self,
            referrer: &AppUrl,
            reference: &str,
        ) -> Result<(AppUrl, String), ScriptError> {
            let url = referrer
                .resolve(reference)
                .map_err(|error| runtime_error(error.to_string()))?;
            let source = self
                .modules
                .get(url.path())
                .cloned()
                .ok_or_else(|| runtime_error(format!("missing module {}", url.path())))?;
            Ok((url, source))
        }
    }

    #[test]
    fn executes_a_static_module_graph_with_named_and_namespace_imports() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut loader = StaticModuleLoader {
            modules: BTreeMap::from([(
                "/message.js".to_string(),
                r#"export const message = "from module"; export function suffix(value) { return value + " ready"; }"#.to_string(),
            )]),
        };
        let mut runtime = ScriptRuntime::new(document.clone());
        let namespace = runtime
            .execute_module(
                r#"import { message, suffix as addSuffix } from "./message.js"; import * as messages from "./message.js"; document.getElementById("status").textContent = addSuffix(messages.message) + "|" + message; export { message };"#,
                base,
                &mut loader,
            )
            .expect("module graph should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("from module ready|from module")
        );
        assert_eq!(
            namespace
                .values
                .get("message")
                .expect("message export should exist")
                .string_value(),
            "from module"
        );
    }

    #[test]
    fn imports_default_exports_alongside_named_exports() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut loader = StaticModuleLoader {
            modules: BTreeMap::from([(
                "/default.js".to_string(),
                r#"export default "primary"; export const named = "secondary";"#.to_string(),
            )]),
        };
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute_module(
                r#"import primary, { named } from "./default.js"; document.getElementById("status").textContent = primary + "|" + named;"#,
                base,
                &mut loader,
            )
            .expect("default import should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("primary|secondary")
        );
    }

    #[test]
    fn follows_named_re_exports_through_a_packaged_barrel() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut loader = StaticModuleLoader {
            modules: BTreeMap::from([
                (
                    "/barrel.js".to_string(),
                    r#"export { message as label } from "./source.js";"#.to_string(),
                ),
                (
                    "/source.js".to_string(),
                    r#"export const message = "re-exported";"#.to_string(),
                ),
            ]),
        };
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute_module(
                r#"import { label } from "./barrel.js"; document.getElementById("status").textContent = label;"#,
                base,
                &mut loader,
            )
            .expect("named re-export should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("re-exported")
        );
    }

    #[test]
    fn forwards_export_star_names_without_overriding_explicit_exports() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut loader = StaticModuleLoader {
            modules: BTreeMap::from([
                (
                    "/barrel.js".to_string(),
                    r#"export * from "./source.js"; export const message = "explicit";"#.to_string(),
                ),
                (
                    "/source.js".to_string(),
                    r#"export default "hidden"; export const message = "forwarded"; export const count = 2;"#.to_string(),
                ),
            ]),
        };
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute_module(
                r#"import { message, count } from "./barrel.js"; document.getElementById("status").textContent = message + "|" + count;"#,
                base,
                &mut loader,
            )
            .expect("export-star should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("explicit|2")
        );
    }

    #[test]
    fn keeps_named_and_namespace_imports_live_after_source_updates() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut loader = StaticModuleLoader {
            modules: BTreeMap::from([
                (
                    "/barrel.js".to_string(),
                    r#"export { count, increment } from "./counter.js";"#.to_string(),
                ),
                (
                    "/counter.js".to_string(),
                    r#"export let count = 1; export function increment() { count = count + 1; }"#
                        .to_string(),
                ),
            ]),
        };
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute_module(
                r#"
                import { count, increment } from "./barrel.js";
                import * as counter from "./barrel.js";
                const status = document.getElementById("status");
                status.textContent = count + "|" + counter.count;
                increment();
                status.textContent = status.textContent + "|" + count + "|" + counter.count;
                "#,
                base,
                &mut loader,
            )
            .expect("live module bindings should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("1|1|2|2")
        );
    }

    #[test]
    fn rejects_assignment_to_an_imported_live_binding() {
        let document = document("<p>module</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut loader = StaticModuleLoader {
            modules: BTreeMap::from([(
                "/counter.js".to_string(),
                "export let count = 1;".to_string(),
            )]),
        };
        let mut runtime = ScriptRuntime::new(document);
        let error = match runtime.execute_module(
            r#"import { count } from "./counter.js"; count = 2;"#,
            base,
            &mut loader,
        ) {
            Ok(_) => panic!("imported bindings should be read-only"),
            Err(error) => error,
        };
        assert_eq!(error.message, "cannot assign to imported binding count");
    }

    #[test]
    fn dynamically_imports_a_packaged_module_from_a_classic_script() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/dynamic.js",
                "text/javascript; charset=utf-8",
                b"import { suffix } from \"./dynamic-child.js\"; export const message = suffix(\"loaded\"); export const count = 2;".to_vec(),
            )
            .expect("module resource should be accepted");
        resources
            .insert(
                "/dynamic-child.js",
                "text/javascript",
                b"export function suffix(value) { return value; }".to_vec(),
            )
            .expect("child module resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                import("./dynamic.js").then((module) => {
                    status.textContent = module.message + "|" + module.count;
                });
                "#,
            )
            .expect("classic dynamic import should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("loaded|2")
        );
    }

    #[test]
    fn dynamically_imports_a_json_module_with_explicit_attributes() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/config.json",
                "application/json; charset=utf-8",
                br#"{"label":"json","count":2}"#.to_vec(),
            )
            .expect("JSON resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                import("./config.json", { with: { type: "json" } }).then((module) => {
                    status.textContent = module.default.label + "|" + module.default.count;
                });
                "#,
            )
            .expect("JSON dynamic import should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("json|2")
        );
    }

    #[test]
    fn rejects_unsupported_dynamic_import_attributes() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/config.json",
                "application/json",
                br#"{"label":"json"}"#.to_vec(),
            )
            .expect("JSON resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                import("./config.json", { with: { type: "css" } }).catch((error) => {
                    status.textContent = error;
                });
                "#,
            )
            .expect("unsupported JSON type should stay a rejected promise");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("unsupported dynamic import type css")
        );
    }

    #[test]
    fn awaits_a_dynamic_import_and_reports_rejections() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/dynamic.js",
                "application/javascript",
                b"export const message = \"async\"; export const count = 3;".to_vec(),
            )
            .expect("module resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                async function load() {
                    const { message, count } = await import("./dynamic.js");
                    return message + "|" + count;
                }
                load().then((value) => {
                    status.textContent = value;
                });
                import("./missing.js").catch((error) => {
                    status.textContent = status.textContent + "|" + error;
                });
                import("https://other.invalid/dynamic.js").catch((error) => {
                    status.textContent = status.textContent + "|" + error;
                });
                "#,
            )
            .expect("async dynamic import should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some(
                "async|3|module /missing.js was not found|URL is outside the app origin",
            )
        );
    }

    #[test]
    fn rejects_cyclic_module_graphs() {
        let document = document("<p>module</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut loader = StaticModuleLoader {
            modules: BTreeMap::from([
                (
                    "/a.js".to_string(),
                    "import \"./b.js\"; export const a = 1;".to_string(),
                ),
                (
                    "/b.js".to_string(),
                    "import \"./a.js\"; export const b = 2;".to_string(),
                ),
            ]),
        };
        let mut runtime = ScriptRuntime::new(document);
        let error = match runtime.execute_module("import \"./a.js\";", base, &mut loader) {
            Ok(_) => panic!("cyclic graph should be rejected"),
            Err(error) => error,
        };
        assert!(error.message.contains("cyclic module import"));
    }

    #[test]
    fn evaluates_template_interpolation_in_classic_scripts() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const count = 3;
                document.getElementById("status").textContent = `ready ${count}|${1 + 2}`;
                "#,
            )
            .expect("template literal should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("ready 3|3")
        );
    }

    #[test]
    fn exposes_import_meta_url_to_modules() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/entry.js")
            .expect("module URL should parse");
        let mut loader = StaticModuleLoader {
            modules: BTreeMap::new(),
        };
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute_module(
                r#"document.getElementById("status").textContent = `loaded ${new URL(import.meta.url).pathname}`;"#,
                base,
                &mut loader,
            )
            .expect("import.meta should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("loaded /entry.js")
        );
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
    fn builds_a_map_from_selector_results_and_dataset_values() {
        let document = document(
            "<main><p data-check='module' id='module'>module</p><p data-check='template' id='template'>template</p><p id='status'>idle</p></main>",
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const checks = new Map(
                  [...document.querySelectorAll("[data-check]")].map((check) => [
                    check.dataset.check,
                    check,
                  ]),
                );
                const status = document.getElementById("status");
                status.textContent = checks.get("module").id + "|" + checks.size + "|" + checks.keys().length;
                "##,
            )
            .expect("collection expression should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("module|2|2")
        );
    }

    #[test]
    fn iterates_map_keys_with_for_of() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const checks = new Map([["module", 1], ["storage", 2]]);
                let output = "";
                for (const name of checks.keys()) {
                  output = output + name + "|";
                }
                document.getElementById("status").textContent = output;
                "##,
            )
            .expect("for-of should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("module|storage|")
        );
    }

    #[test]
    fn destructures_objects_arrays_and_function_parameters() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const payload = { status: "ready", nested: { count: 2 }, extra: "kept" };
                const { status, nested: { count }, ...rest } = payload;
                const [first, , third, ...tail] = ["a", "b", "c", "d"];
                const format = ({ status }, [first]) => status + "|" + first;
                let pairs = "";
                for (const [key, value] of [["x", 1], ["y", 2]]) {
                  pairs = pairs + key + value;
                }
                document.getElementById("status").textContent =
                  format(payload, [first]) + "|" + count + "|" + rest.extra + "|" + third + "|" + tail.length + "|" + pairs;
                "##,
            )
            .expect("destructuring should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("ready|a|2|kept|c|1|x1y2")
        );
    }

    #[test]
    fn applies_defaults_to_missing_destructured_values_and_parameters() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const payload = { nested: {}, keep: "yes" };
                const { label = "fallback", nested: { count = 2 } = {}, ...rest } = payload;
                const [first = "first", second = first + " second", , fourth = "fourth"] = ["one"];
                function describe({ name = "guest" } = {}) {
                  return name;
                }
                let seen = "";
                for (const { value = "missing" } of [{}, { value: "present" }]) {
                  seen = seen + value + "|";
                }
                document.getElementById("status").textContent =
                  label + "|" + count + "|" + rest.keep + "|" + first + "|" + second + "|" + fourth + "|" + describe() + "|" + describe({ name: "Ceri" }) + "|" + seen;
                "##,
            )
            .expect("destructuring defaults should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("fallback|2|yes|one|one second|fourth|guest|Ceri|missing|present|")
        );
    }

    #[test]
    fn resolves_computed_object_pattern_keys_and_excludes_them_from_rest() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const payload = { status: "ready", count: 2, keep: "yes" };
                const statusKey = "status";
                const countKey = "count";
                const { [statusKey]: status, [countKey]: count, ...rest } = payload;
                document.getElementById("status").textContent =
                  status + "|" + count + "|" + rest.keep + "|" + rest.status + "|" + rest.count;
                "##,
            )
            .expect("computed object keys should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("ready|2|yes|undefined|undefined")
        );
    }

    #[test]
    fn iterates_user_defined_symbol_iterators() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const values = ["a", "b", "c"];
                let index = 0;
                const iterable = {
                  [Symbol.iterator]() {
                    return {
                      next() {
                        if (index < values.length) {
                          const value = values[index];
                          index = index + 1;
                          return { value: value, done: false };
                        }
                        return { done: true };
                      }
                    };
                  }
                };
                let output = "";
                for (const value of iterable) {
                  output = output + value;
                }
                document.getElementById("status").textContent = output;
                "##,
            )
            .expect("user-defined iterator should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("abc")
        );
    }

    #[test]
    fn rejects_user_defined_iterators_that_never_finish() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document);
        let error = runtime
            .execute(
                r##"
                const iterable = {
                  [Symbol.iterator]() {
                    return { next() { return { value: 1, done: false }; } };
                  }
                };
                [...iterable];
                "##,
            )
            .expect_err("an endless iterator should hit the deterministic limit");
        assert!(error
            .message
            .contains("user-defined iterator exceeded the deterministic limit"));
    }

    #[test]
    fn map_key_iterators_expose_next_records() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const iterator = new Map([["module", 1], ["storage", 2]]).keys();
                const { value: first, done: firstDone } = iterator.next();
                const { value: second, done: secondDone } = iterator.next();
                const { value: third, done: thirdDone } = iterator.next();
                document.getElementById("status").textContent =
                  first + "|" + firstDone + "|" + second + "|" + secondDone + "|" +
                  thirdDone + "|" + iterator.length;
                "##,
            )
            .expect("iterator records should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("module|false|storage|false|true|0")
        );
    }

    #[test]
    fn iterates_map_values_entries_and_set_values() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const map = new Map([["a", 1], ["b", 2]]);
                const values = [...map.values()];
                const entries = [...map.entries()];
                const setValues = [...new Set(["a", "a", "b"]).values()];
                document.getElementById("status").textContent =
                  values.join(",") + "|" +
                  entries.map(([key, value]) => key + value).join(",") + "|" +
                  setValues.join(",");
                "##,
            )
            .expect("map and set iterators should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("1,2|a1,b2|a,b")
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
    fn focus_and_text_input_update_controls_and_events() {
        let document = document(
            r#"<input id="name" value="R"><p id="status">idle</p>"#,
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const input = document.getElementById("name");
                const status = document.getElementById("status");
                input.addEventListener("focus", () => {
                    status.textContent = "focus:" + document.activeElement.id;
                });
                input.addEventListener("input", (event) => {
                    status.textContent = event.data + ":" + event.target.value;
                });
                input.addEventListener("blur", () => {
                    status.textContent = status.textContent + "|blur:" + document.activeElement;
                });
                "#,
            )
            .expect("focus listeners should register");
        let input_path = document
            .borrow()
            .find_element_by_id("name")
            .expect("input should exist");
        assert!(runtime
            .focus_path(input_path)
            .expect("input should focus"));
        assert!(runtime.insert_text("ust").expect("text should insert"));
        assert!(runtime
            .key_down("Backspace")
            .expect("backspace should edit the value"));
        assert!(runtime.blur_focused().expect("input should blur"));
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some(":Rus|blur:null")
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
    fn storage_exposes_standard_key_value_operations() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                localStorage.setItem("theme", "night");
                localStorage.setItem("count", 2);
                const before = localStorage.length + ":" + localStorage.getItem("theme") + ":" + localStorage.key(0);
                localStorage.removeItem("count");
                sessionStorage.setItem("draft", "open");
                const remaining = localStorage.length;
                const draft = sessionStorage.getItem("draft");
                localStorage.clear();
                sessionStorage.clear();
                document.getElementById("status").textContent = before + ":" + remaining + ":" + draft + ":" + localStorage.length + ":" + sessionStorage.length;
                "#,
            )
            .expect("storage APIs should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("2:night:count:1:open:0:0")
        );
    }

    #[test]
    fn form_data_collects_named_controls_and_dispatches_submit() {
        let document = document(
            r#"<form id="profile"><input name="name" value="Ceri"><input name="tag" value="one"><input name="tag" value="two"><input name="disabled" value="skip" disabled><input name="check" type="checkbox" value="yes"><textarea name="note">draft</textarea></form><p id="status">idle</p>"#,
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const form = document.getElementById("profile");
                const data = new FormData(form);
                data.append("tag", "three");
                const first = data.get("tag");
                const tags = data.getAll("tag");
                const hasNote = data.has("note");
                data.set("name", "Updated");
                data.delete("note");
                const empty = new FormData();
                empty.append("empty", "ok");
                form.addEventListener("submit", (event) => {
                    event.preventDefault();
                    document.getElementById("status").textContent = data.get("name") + "|" + first + "|" + tags.length + "|" + hasNote + "|" + data.has("note") + "|" + data.getAll("tag").length + "|" + empty.get("empty");
                });
                form.requestSubmit();
                "#,
            )
            .expect("FormData APIs should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("Updated|one|3|true|false|3|ok")
        );
    }

    #[test]
    fn checkbox_click_default_updates_live_form_state() {
        let document = document(
            r#"<form id="profile"><input id="accepted" name="accepted" type="checkbox" value="yes"><input id="veto" name="veto" type="checkbox" value="yes" checked><p id="status">idle</p></form>"#,
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const accepted = document.getElementById("accepted");
                const veto = document.getElementById("veto");
                let changed = false;
                accepted.addEventListener("change", () => { changed = accepted.checked; });
                veto.addEventListener("click", (event) => event.preventDefault());
                accepted.click();
                veto.click();
                const data = new FormData(document.getElementById("profile"));
                document.getElementById("status").textContent = accepted.checked + "|" + veto.checked + "|" + changed + "|" + data.has("accepted") + "|" + data.has("veto");
                "#,
            )
            .expect("checkbox click defaults should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("true|true|true|true|true")
        );
    }

    #[test]
    fn radio_click_default_keeps_one_group_value() {
        let document = document(
            r#"<form id="profile"><input id="first" name="mode" type="radio" value="first" checked><input id="second" name="mode" type="radio" value="second"><p id="status">idle</p></form>"#,
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                document.getElementById("second").click();
                const first = document.getElementById("first");
                const second = document.getElementById("second");
                const data = new FormData(document.getElementById("profile"));
                document.getElementById("status").textContent = first.checked + "|" + second.checked + "|" + data.get("mode");
                "#,
            )
            .expect("radio click defaults should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("false|true|second")
        );
    }

    #[test]
    fn canvas_2d_context_records_fill_and_clear_commands() {
        let document = document(
            r#"<canvas id="art" style="display: block; width: 80px; height: 60px"></canvas><p id="status">idle</p>"#,
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const canvas = document.getElementById("art");
                const ctx = canvas.getContext("2d");
                ctx.fillStyle = "#ff0000";
                ctx.fillRect(4, 5, 10, 8);
                ctx.clearRect(6, 7, 2, 2);
                document.getElementById("status").textContent = ctx.fillStyle;
                "##,
            )
            .expect("canvas APIs should execute");
        assert_eq!(runtime.canvas_commands().len(), 2);
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("#ff0000")
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
    fn await_drains_a_fulfilled_promise_chain() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                async function readStatus() {
                  return await Promise.resolve("ready").then((value) => value + "!");
                }
                readStatus().then((value) => { status.textContent = value; });
                "#,
            )
            .expect("await should drain a fulfilled promise chain");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("ready!")
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
