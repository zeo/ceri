use crate::dom::{AccessibilityNode, Document, Element, Node, NodePath};
use crate::css::StyleState;
use crate::navigation::{
    is_http_redirect, is_javascript, is_json, AppUrl, ClipboardProvider, FilePicker,
    FilePickerRequest, NavigationError, Resource, ResourceBundle, ResourceCacheMode,
    normalize_headers, ResourceProvider, ResourceRequest, ResourceResponse,
    APP_ORIGIN,
};
use crate::paint::CanvasCommand;
use crate::Color;
use crate::wasm;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::rc::Rc;
use std::rc::Weak;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

type EnvironmentRef = Rc<RefCell<Environment>>;
type ElementHandle = Rc<RefCell<ElementLocation>>;
type ValidityHandle = Rc<NodePath>;
type RangeHandle = Rc<RefCell<RangeData>>;
type SelectionHandle = Rc<RefCell<SelectionData>>;
type RequestHandle = Rc<RequestData>;
type ResponseHandle = Rc<ResponseData>;
type ResponseBodyHandle = Rc<RefCell<ResponseBodyData>>;
type ReadableStreamHandle = Rc<RefCell<ReadableStreamData>>;
type ReadableStreamReaderHandle = Rc<RefCell<ReadableStreamReaderData>>;
type HeadersHandle = Rc<RefCell<HeadersData>>;
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
type BlobHandle = Rc<BlobData>;
type FileHandle = Rc<FileData>;
type FileListHandle = Rc<Vec<FileHandle>>;
type CanvasContextHandle = Rc<RefCell<CanvasContextData>>;
type AbortSignalHandle = Rc<RefCell<AbortSignalData>>;
type AbortControllerHandle = Rc<RefCell<AbortControllerData>>;
type BytesHandle = Rc<Vec<u8>>;
type WorkerHandle = Rc<RefCell<WorkerData>>;
type WasmFunctionHandle = Rc<WasmFunctionData>;

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
    Validity(ValidityHandle),
    Range(RangeHandle),
    Selection(SelectionHandle),
    Dataset(ElementHandle),
    Error(ErrorHandle),
    Style(ElementHandle),
    ClassList(ElementHandle),
    Event(EventData),
    Response(ResponseHandle),
    ResponseConstructor,
    Bytes(BytesHandle),
    ReadableStream(ReadableStreamHandle),
    ReadableStreamReader(ReadableStreamReaderHandle),
    Headers(HeadersHandle),
    Promise(PromiseHandle),
    Document,
    Window,
    Location(LocationHandle),
    Navigator,
    Clipboard,
    ModuleImport(AppUrl),
    History(HistoryHandle),
    Url(UrlHandle),
    UrlSearchParams(UrlSearchParamsHandle),
    Storage(StorageHandle),
    FormData(FormDataHandle),
    Blob(BlobHandle),
    File(FileHandle),
    FileList(FileListHandle),
    CanvasContext(CanvasContextHandle),
    AbortController(AbortControllerHandle),
    AbortSignal(AbortSignalHandle),
    Worker(WorkerHandle),
    Console,
    Json,
    WebAssemblyNamespace,
    Request(RequestHandle),
    HeadersConstructor,
    RequestConstructor,
    UrlConstructor,
    UrlSearchParamsConstructor,
    FormDataConstructor,
    BlobConstructor,
    FileConstructor,
    MapConstructor,
    SetConstructor,
    ErrorConstructor,
    PromiseConstructor,
    AbortControllerConstructor,
    WorkerConstructor,
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
    DocumentCreateRange,
    DocumentGetSelection,
    DocumentAddEventListener,
    ConsoleLog,
    EventPreventDefault(Rc<RefCell<EventState>>),
    LocationAssign,
    LocationReplace,
    WindowAddEventListener,
    WindowScrollTo,
    WindowScrollBy,
    WindowGetSelection,
    ClipboardReadText,
    ClipboardWriteText,
    HistoryPushState,
    HistoryReplaceState,
    HistoryBack,
    HistoryForward,
    HistoryGo,
    ElementFocus(NodePath),
    ElementBlur(NodePath),
    ElementRequestSubmit(NodePath),
    ElementCheckValidity(NodePath),
    ElementReportValidity(NodePath),
    ElementSetCustomValidity(NodePath),
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
    UrlSearchParamsKeys(UrlSearchParamsHandle),
    UrlSearchParamsValues(UrlSearchParamsHandle),
    UrlSearchParamsEntries(UrlSearchParamsHandle),
    UrlSearchParamsForEach(UrlSearchParamsHandle),
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
    FormDataKeys(FormDataHandle),
    FormDataValues(FormDataHandle),
    FormDataEntries(FormDataHandle),
    FormDataForEach(FormDataHandle),
    BlobSlice(BlobHandle),
    BlobText(BlobHandle),
    BlobBytes(BlobHandle),
    FileSlice(FileHandle),
    FileText(FileHandle),
    FileBytes(FileHandle),
    FileListItem(FileListHandle),
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
    ElementSelect(NodePath),
    ElementSetSelectionRange(NodePath),
    ElementSetRangeText(NodePath),
    RangeSetStart(RangeHandle),
    RangeSetEnd(RangeHandle),
    RangeCollapse(RangeHandle),
    RangeSelectNodeContents(RangeHandle),
    RangeClone(RangeHandle),
    RangeToString(RangeHandle),
    RangeDetach,
    SelectionAddRange(SelectionHandle),
    SelectionRemoveAllRanges(SelectionHandle),
    SelectionGetRangeAt(SelectionHandle),
    SelectionCollapse(SelectionHandle),
    SelectionToString(SelectionHandle),
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
    ResponseErrorConstructor,
    ResponseJsonConstructor,
    ResponseRedirectConstructor,
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
    RequestClone(RequestHandle),
    RequestText(RequestHandle),
    RequestBytes(RequestHandle),
    RequestBlob(RequestHandle),
    RequestFormData(RequestHandle),
    HeadersAppend(HeadersHandle),
    HeadersDelete(HeadersHandle),
    HeadersGet(HeadersHandle),
    HeadersHas(HeadersHandle),
    HeadersKeys(HeadersHandle),
    HeadersValues(HeadersHandle),
    HeadersEntries(HeadersHandle),
    HeadersForEach(HeadersHandle),
    HeadersSet(HeadersHandle),
    ResponseClone(ResponseHandle),
    ResponseText(ResponseHandle),
    ResponseJson(ResponseHandle),
    ResponseBytes(ResponseHandle),
    ResponseBlob(ResponseHandle),
    ResponseFormData(ResponseHandle),
    ReadableStreamGetReader(ReadableStreamHandle),
    ReadableStreamCancel(ReadableStreamHandle),
    ReadableStreamTee(ReadableStreamHandle),
    ReadableStreamReaderRead(ReadableStreamReaderHandle),
    ReadableStreamReaderReleaseLock(ReadableStreamReaderHandle),
    ReadableStreamReaderCancel(ReadableStreamReaderHandle),
    AbortControllerAbort(AbortControllerHandle),
    AbortSignalAddEventListener(AbortSignalHandle),
    AbortSignalThrowIfAborted(AbortSignalHandle),
    WorkerPostMessage(WorkerHandle),
    WorkerTerminate(WorkerHandle),
    WorkerAddEventListener(WorkerHandle),
    WorkerGlobalImportScripts,
    WorkerGlobalPostMessage(SyncSender<WorkerEvent>),
    WebAssemblyInstantiate,
    WebAssemblyFunction(WasmFunctionHandle),
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
    resource_provider: Option<Rc<dyn ResourceProvider>>,
    base_url: Option<AppUrl>,
    global: EnvironmentRef,
    location: LocationHandle,
    history: HistoryHandle,
    listeners: HashMap<NodePath, Vec<EventListener>>,
    window_listeners: HashMap<String, Vec<Value>>,
    focused_element: Option<NodePath>,
    control_values: HashMap<NodePath, String>,
    control_checked: HashMap<NodePath, bool>,
    dirty_controls: BTreeSet<NodePath>,
    last_user_edit: BTreeSet<NodePath>,
    custom_validity: BTreeMap<NodePath, String>,
    text_selections: HashMap<NodePath, TextSelection>,
    composition: Option<TextComposition>,
    selection: SelectionHandle,
    selected_files: HashMap<NodePath, Vec<FileHandle>>,
    file_lists: RefCell<HashMap<NodePath, FileListHandle>>,
    file_picker: Option<Rc<dyn FilePicker>>,
    clipboard_provider: Option<Rc<dyn ClipboardProvider>>,
    scroll_offsets: BTreeMap<NodePath, (f32, f32)>,
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
    workers: Vec<WorkerHandle>,
    worker_sender: Option<SyncSender<WorkerEvent>>,
    worker_script_type: Option<WorkerScriptType>,
    worker_resource_sender: Option<SyncSender<WorkerResourceRequest>>,
    worker_resource_receiver: Option<Receiver<WorkerResourceRequest>>,
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
    provider: Option<&'a dyn ResourceProvider>,
}

struct WorkerResourceRequest {
    request: ResourceRequest,
    reply: SyncSender<Result<Option<ResourceResponse>, NavigationError>>,
}

struct WorkerResourceProvider {
    sender: SyncSender<WorkerResourceRequest>,
}

impl WorkerResourceProvider {
    fn request(
        &self,
        request: ResourceRequest,
    ) -> Result<Option<ResourceResponse>, NavigationError> {
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        self.sender
            .try_send(WorkerResourceRequest {
                request,
                reply: reply_sender,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => NavigationError::ResourceProvider(
                    "worker resource request queue is full".to_string(),
                ),
                TrySendError::Disconnected(_) => NavigationError::ResourceProvider(
                    "worker resource bridge is unavailable".to_string(),
                ),
            })?;
        reply_receiver.recv().map_err(|_| {
            NavigationError::ResourceProvider("worker resource bridge is unavailable".to_string())
        })?
    }
}

impl ResourceProvider for WorkerResourceProvider {
    fn provide(&self, url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
        self.provide_request(&ResourceRequest::get(url.clone()))
    }

    fn provide_request(
        &self,
        request: &ResourceRequest,
    ) -> Result<Option<Resource>, NavigationError> {
        let response = self.request(request.clone())?;
        Ok(response
            .filter(|response| (200..300).contains(&response.status()))
            .map(|response| {
                Resource::new(response.mime_type().to_string(), response.body().to_vec())
            }))
    }

    fn provide_response(
        &self,
        request: &ResourceRequest,
    ) -> Result<Option<ResourceResponse>, NavigationError> {
        self.request(request.clone())
    }
}

fn resolve_runtime_resource(
    resources: &ResourceBundle,
    provider: Option<&dyn ResourceProvider>,
    url: &AppUrl,
) -> Result<Option<Resource>, ScriptError> {
    resolve_runtime_request(resources, provider, &ResourceRequest::get(url.clone()))
}

fn resolve_runtime_request(
    resources: &ResourceBundle,
    provider: Option<&dyn ResourceProvider>,
    request: &ResourceRequest,
) -> Result<Option<Resource>, ScriptError> {
    if request.method() == "GET"
        && let Some(resource) = resources.resource(request.url().path()).cloned()
    {
        return Ok(Some(resource));
    }
    provider
        .map(|provider| provider.provide_request(request))
        .transpose()
        .map(|resource| resource.flatten())
        .map_err(|error| runtime_error(error.to_string()))
}

fn resolve_runtime_response_with_redirects(
    resources: &ResourceBundle,
    provider: Option<&dyn ResourceProvider>,
    request: &ResourceRequest,
    redirect_mode: FetchRedirectMode,
) -> Result<Option<ResolvedRuntimeResponse>, ScriptError> {
    let mut request = request.clone();
    let mut redirected = false;
    for _ in 0..=MAX_FETCH_REDIRECTS {
        let response = if request.method() == "GET"
            && let Some(resource) = resources.resource(request.url().path()).cloned()
        {
            Some(ResourceResponse::from_resource(resource))
        } else {
            provider
                .map(|provider| provider.provide_response(&request))
                .transpose()
                .map(|response| response.flatten())
                .map_err(|error| runtime_error(error.to_string()))?
        };
        let Some(response) = response else {
            return Ok(None);
        };
        let Some(location) = response.headers().get("location") else {
            return Ok(Some(ResolvedRuntimeResponse {
                url: request.url().clone(),
                response,
                redirected,
            }));
        };
        if !is_http_redirect(response.status()) {
            return Ok(Some(ResolvedRuntimeResponse {
                url: request.url().clone(),
                response,
                redirected,
            }));
        }
        match redirect_mode {
            FetchRedirectMode::Manual => {
                return Ok(Some(ResolvedRuntimeResponse {
                    url: request.url().clone(),
                    response,
                    redirected,
                }));
            }
            FetchRedirectMode::Error => {
                return Err(runtime_error("fetch redirect was rejected"));
            }
            FetchRedirectMode::Follow => {}
        }
        let target = request
            .url()
            .resolve(location)
            .map_err(|error| runtime_error(error.to_string()))?;
        request = redirect_request(&request, target, response.status())?;
        redirected = true;
    }
    Err(runtime_error("fetch redirect limit exceeded"))
}

struct ResolvedRuntimeResponse {
    url: AppUrl,
    response: ResourceResponse,
    redirected: bool,
}

const MAX_FETCH_REDIRECTS: usize = 16;

fn redirect_request(
    request: &ResourceRequest,
    target: AppUrl,
    status: u16,
) -> Result<ResourceRequest, ScriptError> {
    let rewrite_to_get = match status {
        303 => !matches!(request.method(), "GET" | "HEAD"),
        301 | 302 => request.method() == "POST",
        _ => false,
    };
    let (method, body) = if rewrite_to_get {
        ("GET", Vec::new())
    } else {
        (request.method(), request.body().to_vec())
    };
    let mut headers = request.headers().clone();
    if rewrite_to_get {
        headers.remove("content-length");
        headers.remove("content-type");
    }
    let cache_mode = request.cache_mode().clone();
    ResourceRequest::new(target, method, headers, body)
        .map(|request| request.with_cache_mode(cache_mode))
        .map_err(|error| runtime_error(error.to_string()))
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
        let resource = resolve_runtime_resource(self.resources, self.provider, &url)?
            .ok_or_else(|| runtime_error(format!("module {} was not found", url.path())))?;
        if !is_javascript(&resource) {
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
    modifiers: KeyModifiers,
    is_composing: bool,
    data_value: Option<Box<Value>>,
    error_message: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool,
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

#[derive(Clone)]
enum FormDataEntry {
    Text(String),
    File(FileHandle),
}

struct FormDataData {
    pairs: Vec<(String, FormDataEntry)>,
}

struct BlobData {
    bytes: Vec<u8>,
    mime_type: String,
}

struct FileData {
    bytes: Vec<u8>,
    mime_type: String,
    name: String,
    last_modified: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SelectionDirection {
    #[default]
    None,
    Forward,
    Backward,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TextSelection {
    start: usize,
    end: usize,
    direction: SelectionDirection,
}

#[derive(Clone, Debug)]
struct TextComposition {
    path: NodePath,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct ConstraintFlags {
    value_missing: bool,
    type_mismatch: bool,
    pattern_mismatch: bool,
    too_long: bool,
    too_short: bool,
    range_underflow: bool,
    range_overflow: bool,
    step_mismatch: bool,
    bad_input: bool,
    custom_error: bool,
}

impl ConstraintFlags {
    fn valid(self) -> bool {
        !self.value_missing
            && !self.type_mismatch
            && !self.pattern_mismatch
            && !self.too_long
            && !self.too_short
            && !self.range_underflow
            && !self.range_overflow
            && !self.step_mismatch
            && !self.bad_input
            && !self.custom_error
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RangeBoundary {
    path: NodePath,
    offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RangeData {
    start: RangeBoundary,
    end: RangeBoundary,
}

#[derive(Clone, Debug, Default)]
struct SelectionData {
    range: Option<RangeHandle>,
}

struct HeadersData {
    values: BTreeMap<String, String>,
    mutable: bool,
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

struct AbortSignalData {
    aborted: bool,
    reason: Option<Value>,
    listeners: Vec<Value>,
}

struct AbortControllerData {
    signal: AbortSignalHandle,
}

const MAX_WORKER_MESSAGES: usize = 256;
const MAX_WORKER_SOURCE_BYTES: usize = 1024 * 1024;

enum WorkerCommand {
    Message(String),
    Terminate,
}

#[derive(Clone, Copy)]
enum WorkerScriptType {
    Classic,
    Module,
}

enum WorkerEvent {
    Message(String),
    Error(String),
}

struct WorkerData {
    url: AppUrl,
    name: String,
    command_sender: SyncSender<WorkerCommand>,
    event_receiver: Receiver<WorkerEvent>,
    onmessage: Option<Value>,
    onerror: Option<Value>,
    listeners: Vec<EventListener>,
    terminated: bool,
    termination_flag: Arc<AtomicBool>,
}

struct WasmFunctionData {
    module: Rc<wasm::Module>,
    function_index: u32,
}

impl Drop for WorkerData {
    fn drop(&mut self) {
        self.termination_flag.store(true, Ordering::Release);
        let _ = self.command_sender.try_send(WorkerCommand::Terminate);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NavigationRequest {
    Navigate {
        reference: String,
        replace_history: bool,
    },
    Submit {
        request: ResourceRequest,
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

struct RequestData {
    url: AppUrl,
    method: String,
    headers: HeadersHandle,
    body: ResponseBodyHandle,
    stream: ReadableStreamHandle,
    cache_mode: ResourceCacheMode,
    redirect: FetchRedirectMode,
    signal: Option<AbortSignalHandle>,
}

struct ResponseData {
    url: String,
    status: u16,
    status_text: String,
    mime_type: String,
    headers: HeadersHandle,
    body: ResponseBodyHandle,
    stream: ReadableStreamHandle,
    redirected: bool,
    response_type: String,
}

struct ResponseMetadata {
    redirected: bool,
    response_type: String,
}

struct ResponseBodyData {
    bytes: Vec<u8>,
    cursor: usize,
    consumed: bool,
    disturbed: bool,
    locked: bool,
}

struct ReadableStreamData {
    body: ResponseBodyHandle,
    canceled: bool,
}

struct ReadableStreamReaderData {
    body: ResponseBodyHandle,
    released: bool,
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
        Self::with_context(document, None, None, None, None, None, None)
    }

    pub fn with_resources(
        document: Rc<RefCell<Document>>,
        resources: ResourceBundle,
        base_url: AppUrl,
    ) -> Self {
        Self::with_context(
            document,
            Some(resources),
            None,
            Some(base_url),
            None,
            None,
            None,
        )
    }

    pub(crate) fn with_resources_and_storage_and_runtime_info(
        document: Rc<RefCell<Document>>,
        resources: ResourceBundle,
        resource_provider: Option<Rc<dyn ResourceProvider>>,
        base_url: AppUrl,
        local_storage: StorageHandle,
        session_storage: StorageHandle,
        host_runtime_info: Option<HostRuntimeInfo>,
    ) -> Self {
        Self::with_context(
            document,
            Some(resources),
            resource_provider,
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
            None,
            Some(local_storage),
            Some(session_storage),
            host_runtime_info,
        )
    }

    fn with_context(
        document: Rc<RefCell<Document>>,
        resources: Option<ResourceBundle>,
        resource_provider: Option<Rc<dyn ResourceProvider>>,
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
                .insert("navigator".to_string(), Value::Navigator);
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
                .insert("WebAssembly".to_string(), Value::WebAssemblyNamespace);
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
            values.values.insert(
                "AbortController".to_string(),
                Value::AbortControllerConstructor,
            );
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
                .insert("Blob".to_string(), Value::BlobConstructor);
            values
                .values
                .insert("File".to_string(), Value::FileConstructor);
            values
                .values
                .insert("Worker".to_string(), Value::WorkerConstructor);
            values
                .values
                .insert("Request".to_string(), Value::RequestConstructor);
            values
                .values
                .insert("Response".to_string(), Value::ResponseConstructor);
            values
                .values
                .insert("Headers".to_string(), Value::HeadersConstructor);
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
            values.values.insert(
                "scrollTo".to_string(),
                Value::Builtin(Builtin::WindowScrollTo),
            );
            values.values.insert(
                "scroll".to_string(),
                Value::Builtin(Builtin::WindowScrollTo),
            );
            values.values.insert(
                "scrollBy".to_string(),
                Value::Builtin(Builtin::WindowScrollBy),
            );
        }
        Self {
            document,
            resources,
            resource_provider,
            base_url,
            global,
            location,
            history,
            listeners: HashMap::new(),
            window_listeners: HashMap::new(),
            focused_element: None,
            control_values: HashMap::new(),
            control_checked: HashMap::new(),
            dirty_controls: BTreeSet::new(),
            last_user_edit: BTreeSet::new(),
            custom_validity: BTreeMap::new(),
            text_selections: HashMap::new(),
            composition: None,
            selection: Rc::new(RefCell::new(SelectionData::default())),
            selected_files: HashMap::new(),
            file_lists: RefCell::new(HashMap::new()),
            file_picker: None,
            clipboard_provider: None,
            scroll_offsets: BTreeMap::new(),
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
            workers: Vec::new(),
            worker_sender: None,
            worker_script_type: None,
            worker_resource_sender: None,
            worker_resource_receiver: None,
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
            Completion::Normal | Completion::Return(_) => {
                self.drain_microtasks()?;
                self.pump_workers().map(|_| ())
            }
        }
    }

    pub(crate) fn set_file_picker(&mut self, file_picker: Option<Rc<dyn FilePicker>>) {
        self.file_picker = file_picker;
    }

    pub(crate) fn set_clipboard_provider(
        &mut self,
        clipboard_provider: Option<Rc<dyn ClipboardProvider>>,
    ) {
        self.clipboard_provider = clipboard_provider;
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
        if self.resources.is_none() && self.resource_provider.is_none() {
            return Err(runtime_error(
                "dynamic import is unavailable without resources",
            ));
        }
        let resources = self
            .resources
            .clone()
            .unwrap_or_default();
        let provider = self.resource_provider.clone();
        let resource = resolve_runtime_resource(&resources, provider.as_deref(), &url)?
            .ok_or_else(|| runtime_error(format!("module {} was not found", url.path())))?;
        match import_type {
            None => {
                if !is_javascript(&resource) {
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
                    provider: provider.as_deref(),
                };
                self.execute_module_graph(&source, url, &mut loader)
            }
            Some(DynamicImportType::Json) => {
                if !is_json(&resource) {
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
        if !dispatch.default_prevented
            && self.pending_navigation.is_none()
            && let Some(control) = self.label_activation_control(&path)
        {
            let activated = self.click_path(control)?;
            return Ok(dispatch.handled || focused || activated);
        }
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

    fn label_activation_control(&self, path: &[usize]) -> Option<NodePath> {
        let document = self.document.borrow();
        let label_path = if document
            .element_clone(path)
            .is_some_and(|element| element.name == "label")
        {
            path.to_vec()
        } else {
            document.label_ancestor_path(path)?
        };
        let control = document.label_control_path(&label_path)?;
        if path.starts_with(&control) {
            return None;
        }
        let start = label_path.len();
        if path.len() > start
            && (start + 1..=path.len()).any(|length| {
                document.element_clone(&path[..length]).is_some_and(|element| {
                    matches!(
                        element.name.as_str(),
                        "a" | "button" | "input" | "select" | "textarea"
                    )
                })
            })
        {
            return None;
        }
        Some(control)
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
        if self.composition.is_some() {
            self.composition_cancel()?;
        }
        let previous = self.focused_element.replace(path.clone());
        if let Some(previous) = previous {
            self.dispatch_event(previous, "blur")?;
            self.drain_microtasks()?;
        }
        if self.is_selection_control_path(&path) && !self.text_selections.contains_key(&path) {
            let value = self.control_value_path(&path).unwrap_or_default();
            let cursor = utf16_len(&value);
            self.text_selections.insert(
                path.clone(),
                TextSelection {
                    start: cursor,
                    end: cursor,
                    direction: SelectionDirection::None,
                },
            );
        }
        self.dispatch_event(path, "focus")?;
        self.drain_microtasks()?;
        Ok(true)
    }

    pub(crate) fn blur_focused(&mut self) -> Result<bool, ScriptError> {
        let composition_changed = self.composition_cancel()?;
        let Some(path) = self.focused_element.take() else {
            return Ok(composition_changed);
        };
        self.dispatch_event(path, "blur")?;
        self.drain_microtasks()?;
        Ok(true)
    }

    pub(crate) fn apply_autofocus(&mut self) -> Result<bool, ScriptError> {
        if self.focused_element.is_some() {
            return Ok(false);
        }
        let candidates = self.document.borrow().autofocus_paths();
        for path in candidates {
            if self.focus_path(path)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn move_focus_sequential(&mut self, backwards: bool) -> Result<bool, ScriptError> {
        let mut entries = Vec::new();
        let mut order = 0usize;
        let tree = self.document.borrow().accessibility_tree();
        collect_sequential_focus_nodes(
            &tree,
            &mut order,
            &mut entries,
            &self.document,
        );
        entries.sort_by_key(|(tabindex, order, _)| {
            if *tabindex > 0 {
                (0, *tabindex, *order)
            } else {
                (1, 0, *order)
            }
        });
        let paths = entries
            .into_iter()
            .map(|(_, _, path)| path)
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return Ok(false);
        }
        let next = match self
            .focused_element
            .as_ref()
            .and_then(|focused| paths.iter().position(|path| path == focused))
        {
            Some(index) if backwards => {
                paths[(index + paths.len() - 1) % paths.len()].clone()
            }
            Some(index) => paths[(index + 1) % paths.len()].clone(),
            None if backwards => paths.last().cloned().unwrap_or_default(),
            None => paths.first().cloned().unwrap_or_default(),
        };
        self.focus_path(next)
    }

    pub(crate) fn insert_text(&mut self, text: &str) -> Result<bool, ScriptError> {
        if self.composition.is_some() {
            return self.composition_commit(text);
        }
        if text.is_empty() {
            return Ok(false);
        }
        let Some(path) = self.focused_element.clone() else {
            return Ok(false);
        };
        if !self.is_text_control_path(&path) {
            return Ok(false);
        }
        if self.is_readonly_path(&path) {
            return Ok(false);
        }
        let value = self.control_value_path(&path).unwrap_or_default();
        let value_units = value.encode_utf16().collect::<Vec<_>>();
        let mut selection = self.selection_for_path(&path, &value);
        if !self.text_selections.contains_key(&path) {
            let cursor = value_units.len();
            selection = TextSelection {
                start: cursor,
                end: cursor,
                direction: SelectionDirection::None,
            };
        }
        let replacement_text = self
            .user_input_max_length(&path)
            .map(|maximum| {
                let occupied = value_units
                    .len()
                    .saturating_sub(selection.end.saturating_sub(selection.start));
                truncate_utf16(text, maximum.saturating_sub(occupied))
            })
            .unwrap_or_else(|| text.to_string());
        if replacement_text.is_empty() {
            return Ok(false);
        }
        let replacement = replacement_text.encode_utf16().collect::<Vec<_>>();
        let mut next_units = Vec::with_capacity(
            value_units
                .len()
                .saturating_sub(selection.end.saturating_sub(selection.start))
                .saturating_add(replacement.len()),
        );
        next_units.extend_from_slice(&value_units[..selection.start]);
        next_units.extend_from_slice(&replacement);
        next_units.extend_from_slice(&value_units[selection.end..]);
        let value = String::from_utf16_lossy(&next_units);
        let cursor = selection.start.saturating_add(replacement.len());
        self.control_values.insert(path.clone(), value);
        self.mark_user_edit(&path);
        self.text_selections.insert(
            path.clone(),
            TextSelection {
                start: cursor,
                end: cursor,
                direction: SelectionDirection::None,
            },
        );
        let dispatch = self.dispatch_event_with_details(path, "input", "", &replacement_text)?;
        self.drain_microtasks()?;
        Ok(dispatch.handled || !replacement_text.is_empty())
    }

    fn begin_composition(&mut self) -> Result<Option<NodePath>, ScriptError> {
        if let Some(composition) = &self.composition {
            return Ok(Some(composition.path.clone()));
        }
        let Some(path) = self.focused_element.clone() else {
            return Ok(None);
        };
        if !self.is_selection_control_path(&path) {
            return Ok(None);
        }
        if self.is_readonly_path(&path) {
            return Ok(None);
        }
        let value = self.control_value_path(&path).unwrap_or_default();
        let mut selection = self.selection_for_path(&path, &value);
        if !self.text_selections.contains_key(&path) {
            let cursor = utf16_len(&value);
            selection = TextSelection {
                start: cursor,
                end: cursor,
                direction: SelectionDirection::None,
            };
        }
        let dispatch = self.dispatch_event_with_details(
            path.clone(),
            "compositionstart",
            "",
            "",
        )?;
        self.drain_microtasks()?;
        if dispatch.default_prevented {
            return Ok(None);
        }
        self.composition = Some(TextComposition {
            path: path.clone(),
            start: selection.start,
            end: selection.end,
        });
        Ok(Some(path))
    }

    pub(crate) fn composition_update(
        &mut self,
        text: &str,
        _cursor: Option<(usize, usize)>,
    ) -> Result<bool, ScriptError> {
        let Some(path) = self.begin_composition()? else {
            return Ok(false);
        };
        let Some(composition) = self.composition.clone() else {
            return Ok(false);
        };
        let update = self.dispatch_event_with_details(
            path.clone(),
            "compositionupdate",
            "",
            text,
        )?;
        self.drain_microtasks()?;
        let value = self.control_value_path(&path).unwrap_or_default();
        let length = utf16_len(&value);
        let start = composition.start.min(length);
        let end = composition.end.min(length).max(start);
        let replacement_text = self
            .user_input_max_length(&path)
            .map(|maximum| {
                let occupied = length.saturating_sub(end.saturating_sub(start));
                truncate_utf16(text, maximum.saturating_sub(occupied))
            })
            .unwrap_or_else(|| text.to_string());
        let (value, selection_end) =
            replace_utf16_range(&value, start, end, &replacement_text);
        self.control_values.insert(path.clone(), value);
        if start != end || !replacement_text.is_empty() {
            self.mark_user_edit(&path);
        }
        self.composition = Some(TextComposition {
            path: path.clone(),
            start,
            end: selection_end,
        });
        self.text_selections.insert(
            path.clone(),
            TextSelection {
                start: selection_end,
                end: selection_end,
                direction: SelectionDirection::None,
            },
        );
        let input = self.dispatch_event_with_details(path, "input", "", &replacement_text)?;
        self.drain_microtasks()?;
        Ok(update.handled || input.handled || start != end || !replacement_text.is_empty())
    }

    pub(crate) fn composition_commit(&mut self, text: &str) -> Result<bool, ScriptError> {
        let Some(composition) = self.composition.clone() else {
            return self.insert_text(text);
        };
        let path = composition.path;
        if !self.is_selection_control_path(&path) {
            self.composition = None;
            return Ok(false);
        }
        if self.is_readonly_path(&path) {
            self.composition = None;
            return Ok(false);
        }
        let value = self.control_value_path(&path).unwrap_or_default();
        let length = utf16_len(&value);
        let start = composition.start.min(length);
        let end = composition.end.min(length).max(start);
        let replacement_text = self
            .user_input_max_length(&path)
            .map(|maximum| {
                let occupied = length.saturating_sub(end.saturating_sub(start));
                truncate_utf16(text, maximum.saturating_sub(occupied))
            })
            .unwrap_or_else(|| text.to_string());
        let (value, cursor) = replace_utf16_range(&value, start, end, &replacement_text);
        self.control_values.insert(path.clone(), value);
        if start != end || !replacement_text.is_empty() {
            self.mark_user_edit(&path);
        }
        self.text_selections.insert(
            path.clone(),
            TextSelection {
                start: cursor,
                end: cursor,
                direction: SelectionDirection::None,
            },
        );
        let input = self.dispatch_event_with_details(
            path.clone(),
            "input",
            "",
            &replacement_text,
        )?;
        self.drain_microtasks()?;
        self.composition = None;
        let end_event = self.dispatch_event_with_details(
            path,
            "compositionend",
            "",
            &replacement_text,
        )?;
        self.drain_microtasks()?;
        Ok(input.handled || end_event.handled || start != end || !replacement_text.is_empty())
    }

    pub(crate) fn composition_cancel(&mut self) -> Result<bool, ScriptError> {
        let Some(composition) = self.composition.clone() else {
            return Ok(false);
        };
        let path = composition.path;
        if !self.is_selection_control_path(&path) {
            self.composition = None;
            return Ok(false);
        }
        let update = self.dispatch_event_with_details(
            path.clone(),
            "compositionupdate",
            "",
            "",
        )?;
        self.drain_microtasks()?;
        self.composition = None;
        let value = self.control_value_path(&path).unwrap_or_default();
        let length = utf16_len(&value);
        let start = composition.start.min(length);
        let end = composition.end.min(length).max(start);
        let (value, cursor) = replace_utf16_range(&value, start, end, "");
        self.control_values.insert(path.clone(), value);
        self.text_selections.insert(
            path.clone(),
            TextSelection {
                start: cursor,
                end: cursor,
                direction: SelectionDirection::None,
            },
        );
        let input = if start != end {
            let input = self.dispatch_event_with_details(path.clone(), "input", "", "")?;
            self.drain_microtasks()?;
            input.handled
        } else {
            false
        };
        let end_event = self.dispatch_event_with_details(path, "compositionend", "", "")?;
        self.drain_microtasks()?;
        Ok(update.handled || input || end_event.handled || start != end)
    }

    pub(crate) fn key_down_with_modifiers(
        &mut self,
        key: &str,
        modifiers: KeyModifiers,
    ) -> Result<bool, ScriptError> {
        let Some(path) = self.focused_element.clone() else {
            return Ok(false);
        };
        let dispatch = self.dispatch_event_with_modifiers(
            path.clone(),
            "keydown",
            key,
            "",
            modifiers,
        )?;
        self.drain_microtasks()?;
        if self.composition.is_some() {
            return Ok(dispatch.handled);
        }
        if key == "Tab" && !modifiers.control && !modifiers.alt && !modifiers.meta {
            if dispatch.default_prevented {
                return Ok(dispatch.handled);
            }
            let moved = self.move_focus_sequential(modifiers.shift)?;
            return Ok(dispatch.handled || moved);
        }
        if dispatch.default_prevented || !self.is_text_control_path(&path) {
            return Ok(dispatch.handled);
        }
        let value = self.control_value_path(&path).unwrap_or_default();
        let units = value.encode_utf16().collect::<Vec<_>>();
        let selection = self.selection_for_path(&path, &value);
        if self.is_readonly_path(&path) && matches!(key, "Backspace" | "Delete" | "Enter") {
            return Ok(dispatch.handled);
        }
        if key == "Enter"
            && self
                .document
                .borrow()
                .element_clone(&path)
                .is_some_and(|element| element.name == "textarea")
        {
            if let Some(maximum) = self.user_input_max_length(&path) {
                let occupied = units
                    .len()
                    .saturating_sub(selection.end.saturating_sub(selection.start));
                if occupied >= maximum {
                    return Ok(dispatch.handled);
                }
            }
            self.mark_user_edit(&path);
            self.set_range_text(
                &path,
                &[
                    Value::String("\n".to_string()),
                    Value::Number(selection.start as f64),
                    Value::Number(selection.end as f64),
                    Value::String("end".to_string()),
                ],
            )?;
            return Ok(true);
        }
        if modifiers.shift {
            let (anchor, focus) = selection_anchor_focus(selection);
            let next_focus = match key {
                "ArrowLeft" if focus > 0 => previous_utf16_boundary(&units, focus),
                "ArrowRight" if focus < units.len() => next_utf16_boundary(&units, focus),
                "Home" => 0,
                "End" => units.len(),
                _ => return Ok(dispatch.handled),
            };
            let direction = if next_focus < anchor {
                SelectionDirection::Backward
            } else if next_focus > anchor {
                SelectionDirection::Forward
            } else {
                SelectionDirection::None
            };
            self.set_text_selection(
                path,
                anchor.min(next_focus),
                anchor.max(next_focus),
                direction,
            )?;
            return Ok(true);
        }
        let (start, end) = match key {
            "Backspace" if selection.start != selection.end => (selection.start, selection.end),
            "Backspace" if selection.start > 0 => (
                previous_utf16_boundary(&units, selection.start),
                selection.start,
            ),
            "Delete" if selection.start != selection.end => (selection.start, selection.end),
            "Delete" if selection.start < units.len() => (
                selection.start,
                next_utf16_boundary(&units, selection.start),
            ),
            "ArrowLeft" if selection.start != selection.end => {
                self.set_text_selection(
                    path,
                    selection.start,
                    selection.start,
                    SelectionDirection::None,
                )?;
                return Ok(true);
            }
            "ArrowLeft" if selection.start > 0 => {
                let cursor = previous_utf16_boundary(&units, selection.start);
                self.set_text_selection(path, cursor, cursor, SelectionDirection::None)?;
                return Ok(true);
            }
            "ArrowRight" if selection.start != selection.end => {
                self.set_text_selection(
                    path,
                    selection.end,
                    selection.end,
                    SelectionDirection::None,
                )?;
                return Ok(true);
            }
            "ArrowRight" if selection.end < units.len() => {
                let cursor = next_utf16_boundary(&units, selection.end);
                self.set_text_selection(path, cursor, cursor, SelectionDirection::None)?;
                return Ok(true);
            }
            "Home" => {
                self.set_text_selection(path, 0, 0, SelectionDirection::None)?;
                return Ok(true);
            }
            "End" => {
                let cursor = units.len();
                self.set_text_selection(path, cursor, cursor, SelectionDirection::None)?;
                return Ok(true);
            }
            _ => return Ok(dispatch.handled),
        };
        if start != end {
            self.mark_user_edit(&path);
            self.set_range_text(
                &path,
                &[
                    Value::String(String::new()),
                    Value::Number(start as f64),
                    Value::Number(end as f64),
                    Value::String("start".to_string()),
                ],
            )?;
            return Ok(true);
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
            scroll_offsets: self.scroll_offsets.clone(),
            control_values: self.control_values.clone().into_iter().collect(),
            text_selections: self
                .text_selections
                .iter()
                .map(|(path, selection)| {
                    (
                        path.clone(),
                        (
                            selection.start,
                            selection.end,
                            selection.direction == SelectionDirection::Backward,
                        ),
                    )
                })
                .collect(),
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

    pub(crate) fn scroll_offset(&self, path: &[usize]) -> (f32, f32) {
        self.scroll_offsets.get(path).copied().unwrap_or_default()
    }

    pub(crate) fn document_scroll_offset(&self) -> (f32, f32) {
        self.scroll_offset(&[])
    }

    pub(crate) fn set_scroll_offset(&mut self, path: NodePath, offset: (f32, f32)) -> bool {
        let offset = (
            if offset.0.is_finite() {
                offset.0.max(0.0)
            } else {
                0.0
            },
            if offset.1.is_finite() {
                offset.1.max(0.0)
            } else {
                0.0
            },
        );
        let changed = self.scroll_offset(&path) != offset;
        if changed {
            self.scroll_offsets.insert(path, offset);
        }
        changed
    }

    pub(crate) fn set_document_scroll_offset(&mut self, offset: (f32, f32)) -> bool {
        self.set_scroll_offset(Vec::new(), offset)
    }

    fn set_scroll_offset_property(
        &mut self,
        handle: &ElementHandle,
        axis: usize,
        value: f64,
    ) -> Result<bool, ScriptError> {
        let Some(path) = attached_path(handle) else {
            return Ok(false);
        };
        let mut offset = self.scroll_offset(&path);
        let value = if value.is_finite() {
            (value as f32).max(0.0)
        } else {
            0.0
        };
        if axis == 0 {
            offset.0 = value;
        } else {
            offset.1 = value;
        }
        let changed = self.set_scroll_offset(path.clone(), offset);
        if changed {
            self.dispatch_path_event(path, "scroll")?;
        }
        Ok(changed)
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
            "file" => self.apply_file_picker(path, &element)?,
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

    fn apply_file_picker(
        &mut self,
        path: &[usize],
        element: &Element,
    ) -> Result<bool, ScriptError> {
        let Some(file_picker) = self.file_picker.clone() else {
            return Ok(false);
        };
        let request = FilePickerRequest::new(
            element.attr("name").unwrap_or_default(),
            element.attr("multiple").is_some(),
            element.attr("accept").map(str::to_string),
        );
        let Some(selected) = file_picker
            .pick_files(&request)
            .map_err(|error| runtime_error(error.to_string()))?
        else {
            return Ok(false);
        };
        if !request.multiple() && selected.len() > 1 {
            return Err(runtime_error(
                "file picker returned multiple files for a single-file control",
            ));
        }
        let files = selected
            .into_iter()
            .map(|file| {
                Rc::new(FileData {
                    bytes: file.bytes().to_vec(),
                    mime_type: file.mime_type().to_string(),
                    name: file.name().to_string(),
                    last_modified: file.last_modified(),
                })
            })
            .collect::<Vec<_>>();
        let changed = match self.selected_files.get(path) {
            Some(current) => !file_lists_equal(current, &files),
            None => !files.is_empty(),
        };
        if !changed {
            return Ok(false);
        }
        let value = files
            .first()
            .map(|file| format!("C:\\fakepath\\{}", file.name))
            .unwrap_or_default();
        self.selected_files.insert(path.to_vec(), files);
        let files = self
            .selected_files
            .get(path)
            .cloned()
            .unwrap_or_default();
        self.file_lists
            .borrow_mut()
            .insert(path.to_vec(), Rc::new(files));
        self.control_values.insert(path.to_vec(), value);
        Ok(true)
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
        let bypass_validation = self
            .document
            .borrow()
            .element_clone(&path)
            .is_some_and(|form| form.attr("novalidate").is_some());
        if !bypass_validation && !self.check_validity_path(&path)? {
            return Ok(());
        }
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

    pub(crate) fn dispatch_window_event(&mut self, event_type: &str) -> Result<bool, ScriptError> {
        let callbacks = self
            .window_listeners
            .get(event_type)
            .cloned()
            .unwrap_or_default();
        let event = Value::Event(EventData {
            event_type: event_type.to_string(),
            target: Vec::new(),
            current_target: Vec::new(),
            state: Rc::new(RefCell::new(EventState::default())),
            history_state: Rc::new(RefCell::new(Value::Null)),
            key: String::new(),
            data: String::new(),
            modifiers: KeyModifiers::default(),
            is_composing: false,
            data_value: None,
            error_message: None,
        });
        for callback in callbacks {
            self.call(callback, vec![event.clone()])?;
        }
        self.drain_microtasks()?;
        Ok(self
            .window_listeners
            .get(event_type)
            .is_some_and(|listeners| !listeners.is_empty()))
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
            modifiers: KeyModifiers::default(),
            is_composing: false,
            data_value: None,
            error_message: None,
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
        self.pump_workers()?;
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
                "createRange" => Ok(Value::Builtin(Builtin::DocumentCreateRange)),
                "getSelection" => Ok(Value::Builtin(Builtin::DocumentGetSelection)),
                "addEventListener" => Ok(Value::Builtin(Builtin::DocumentAddEventListener)),
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
                "navigator" => Ok(Value::Navigator),
                "location" => self
                    .global
                    .borrow()
                    .get("location")
                    .ok_or_else(|| runtime_error("window location is unavailable")),
                "scrollX" | "pageXOffset" => {
                    Ok(Value::Number(self.document_scroll_offset().0 as f64))
                }
                "scrollY" | "pageYOffset" => {
                    Ok(Value::Number(self.document_scroll_offset().1 as f64))
                }
                "history" => self
                    .global
                    .borrow()
                    .get("history")
                    .ok_or_else(|| runtime_error("window history is unavailable")),
                "console" => Ok(Value::Console),
                "getSelection" => Ok(Value::Builtin(Builtin::WindowGetSelection)),
                "WebAssembly" => Ok(Value::WebAssemblyNamespace),
                "addEventListener" => Ok(Value::Builtin(Builtin::WindowAddEventListener)),
                "scroll" | "scrollTo" => Ok(Value::Builtin(Builtin::WindowScrollTo)),
                "scrollBy" => Ok(Value::Builtin(Builtin::WindowScrollBy)),
                "JSON" => Ok(Value::Json),
                "AbortController" => Ok(Value::AbortControllerConstructor),
                "Worker" => Ok(Value::WorkerConstructor),
                "fetch" => Ok(Value::Builtin(Builtin::Fetch)),
                "setTimeout" => Ok(Value::Builtin(Builtin::SetTimeout)),
                "clearTimeout" => Ok(Value::Builtin(Builtin::ClearTimeout)),
                "requestAnimationFrame" => Ok(Value::Builtin(Builtin::RequestAnimationFrame)),
                "cancelAnimationFrame" => Ok(Value::Builtin(Builtin::CancelAnimationFrame)),
                "importScripts"
                    if self.worker_sender.is_some()
                        && matches!(self.worker_script_type, Some(WorkerScriptType::Classic)) =>
                {
                    Ok(Value::Builtin(Builtin::WorkerGlobalImportScripts))
                }
                "postMessage" if self.worker_sender.is_some() => Ok(Value::Builtin(
                    Builtin::WorkerGlobalPostMessage(
                        self.worker_sender
                            .clone()
                            .expect("worker sender checked above"),
                    ),
                )),
                "onmessage" | "onerror" if self.worker_sender.is_some() => Ok(self
                    .global
                    .borrow()
                    .get(property)
                    .unwrap_or(Value::Null)),
                "ceriQuery" => Ok(self
                    .host_runtime_info
                    .as_ref()
                    .map(|_| Value::Builtin(Builtin::CeriQuery))
                    .unwrap_or(Value::Undefined)),
                "cefQuery" | "cefQueryCancel" => Ok(Value::Undefined),
                _ => Err(runtime_error(format!("window has no property {property}"))),
            },
            Value::Navigator => match property {
                "clipboard" => Ok(Value::Clipboard),
                _ => Err(runtime_error(format!(
                    "navigator has no property {property}"
                ))),
            },
            Value::Clipboard => match property {
                "readText" => Ok(Value::Builtin(Builtin::ClipboardReadText)),
                "writeText" => Ok(Value::Builtin(Builtin::ClipboardWriteText)),
                _ => Err(runtime_error(format!(
                    "clipboard has no property {property}"
                ))),
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
                    "files" => self.element_files(&handle),
                    "value" => self
                        .element_value(&handle)
                        .map(Value::String)
                        .unwrap_or_else(|| Value::String(String::new())),
                    "validity" => self.element_validity(&handle),
                    "willValidate" => Value::Bool(
                        path.as_ref()
                            .is_some_and(|path| self.is_validation_candidate(path)),
                    ),
                    "validationMessage" => Value::String(
                        path.as_ref()
                            .map(|path| self.validation_message(path))
                            .unwrap_or_default(),
                    ),
                    "required" => Value::Bool(
                        self.element_attribute(&handle, "required").is_some(),
                    ),
                    "maxLength" => Value::Number(
                        path.as_ref()
                            .and_then(|path| self.control_length_attribute(path, "maxlength"))
                            .map(|length| length as f64)
                            .unwrap_or(-1.0),
                    ),
                    "minLength" => Value::Number(
                        path.as_ref()
                            .and_then(|path| self.control_length_attribute(path, "minlength"))
                            .map(|length| length as f64)
                            .unwrap_or(-1.0),
                    ),
                    "selectionStart" => self.element_selection_start(&handle)?,
                    "selectionEnd" => self.element_selection_end(&handle)?,
                    "selectionDirection" => self.element_selection_direction(&handle)?,
                    "checked" => Value::Bool(self.element_checked(&handle)),
                    "disabled" => Value::Bool(
                        self.element_attribute(&handle, "disabled").is_some(),
                    ),
                    "readOnly" => Value::Bool(
                        self.element_attribute(&handle, "readonly").is_some(),
                    ),
                    "scrollLeft" => Value::Number(
                        path.as_ref()
                            .map(|path| self.scroll_offset(path).0 as f64)
                            .unwrap_or(0.0),
                    ),
                    "scrollTop" => Value::Number(
                        path.as_ref()
                            .map(|path| self.scroll_offset(path).1 as f64)
                            .unwrap_or(0.0),
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
                    "select" => Value::Builtin(Builtin::ElementSelect(
                        path.ok_or_else(|| runtime_error("detached element cannot be selected"))?,
                    )),
                    "setSelectionRange" => Value::Builtin(Builtin::ElementSetSelectionRange(
                        path.ok_or_else(|| {
                            runtime_error("detached element cannot set a selection range")
                        })?,
                    )),
                    "setRangeText" => Value::Builtin(Builtin::ElementSetRangeText(
                        path.ok_or_else(|| {
                            runtime_error("detached element cannot replace selected text")
                        })?,
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
                    "checkValidity" => Value::Builtin(Builtin::ElementCheckValidity(
                        path.ok_or_else(|| {
                            runtime_error("detached element cannot validate constraints")
                        })?,
                    )),
                    "reportValidity" => Value::Builtin(Builtin::ElementReportValidity(
                        path.ok_or_else(|| {
                            runtime_error("detached element cannot validate constraints")
                        })?,
                    )),
                    "setCustomValidity" => Value::Builtin(Builtin::ElementSetCustomValidity(
                        path.ok_or_else(|| {
                            runtime_error("detached element cannot set custom validity")
                        })?,
                    )),
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
            Value::Validity(path) => {
                let flags = self.constraint_flags(path.as_slice());
                match property {
                    "badInput" => Ok(Value::Bool(flags.bad_input)),
                    "customError" => Ok(Value::Bool(flags.custom_error)),
                    "patternMismatch" => Ok(Value::Bool(flags.pattern_mismatch)),
                    "rangeOverflow" => Ok(Value::Bool(flags.range_overflow)),
                    "rangeUnderflow" => Ok(Value::Bool(flags.range_underflow)),
                    "stepMismatch" => Ok(Value::Bool(flags.step_mismatch)),
                    "tooLong" => Ok(Value::Bool(flags.too_long)),
                    "tooShort" => Ok(Value::Bool(flags.too_short)),
                    "typeMismatch" => Ok(Value::Bool(flags.type_mismatch)),
                    "valid" => Ok(Value::Bool(flags.valid())),
                    "valueMissing" => Ok(Value::Bool(flags.value_missing)),
                    _ => Err(runtime_error(format!(
                        "ValidityState has no property {property}"
                    ))),
                }
            }
            Value::Range(range) => {
                let range_data = range.borrow();
                match property {
                    "startContainer" => Ok(node_value(range_data.start.path.clone())),
                    "startOffset" => Ok(Value::Number(range_data.start.offset as f64)),
                    "endContainer" => Ok(node_value(range_data.end.path.clone())),
                    "endOffset" => Ok(Value::Number(range_data.end.offset as f64)),
                    "collapsed" => Ok(Value::Bool(range_data.start.path == range_data.end.path
                        && range_data.start.offset == range_data.end.offset)),
                    "commonAncestorContainer" => {
                        let path = common_path(&range_data.start.path, &range_data.end.path);
                        Ok(if path.is_empty() {
                            Value::Document
                        } else {
                            Value::Element(attached_handle(path))
                        })
                    }
                    "setStart" => Ok(Value::Builtin(Builtin::RangeSetStart(range.clone()))),
                    "setEnd" => Ok(Value::Builtin(Builtin::RangeSetEnd(range.clone()))),
                    "collapse" => Ok(Value::Builtin(Builtin::RangeCollapse(range.clone()))),
                    "selectNodeContents" => Ok(Value::Builtin(
                        Builtin::RangeSelectNodeContents(range.clone()),
                    )),
                    "cloneRange" => Ok(Value::Builtin(Builtin::RangeClone(range.clone()))),
                    "toString" => Ok(Value::Builtin(Builtin::RangeToString(range.clone()))),
                    "detach" => Ok(Value::Builtin(Builtin::RangeDetach)),
                    _ => Err(runtime_error(format!("Range has no property {property}"))),
                }
            }
            Value::Selection(selection) => {
                let selection_data = selection.borrow();
                let range = selection_data.range.clone();
                match property {
                    "rangeCount" => Ok(Value::Number(if range.is_some() { 1.0 } else { 0.0 })),
                    "isCollapsed" => Ok(Value::Bool(
                        range
                            .as_ref()
                            .is_none_or(|range| range_is_collapsed(&range.borrow())),
                    )),
                    "anchorNode" | "focusNode" => Ok(range
                        .as_ref()
                        .map(|range| {
                            let range = range.borrow();
                            let boundary = if property == "anchorNode" {
                                &range.start
                            } else {
                                &range.end
                            };
                            node_value(boundary.path.clone())
                        })
                        .unwrap_or(Value::Null)),
                    "anchorOffset" | "focusOffset" => Ok(range
                        .as_ref()
                        .map(|range| {
                            let range = range.borrow();
                            let boundary = if property == "anchorOffset" {
                                &range.start
                            } else {
                                &range.end
                            };
                            Value::Number(boundary.offset as f64)
                        })
                        .unwrap_or(Value::Number(0.0))),
                    "addRange" => Ok(Value::Builtin(Builtin::SelectionAddRange(
                        selection.clone(),
                    ))),
                    "removeAllRanges" => Ok(Value::Builtin(Builtin::SelectionRemoveAllRanges(
                        selection.clone(),
                    ))),
                    "getRangeAt" => Ok(Value::Builtin(Builtin::SelectionGetRangeAt(
                        selection.clone(),
                    ))),
                    "collapse" => Ok(Value::Builtin(Builtin::SelectionCollapse(
                        selection.clone(),
                    ))),
                    "toString" => Ok(Value::Builtin(Builtin::SelectionToString(
                        selection.clone(),
                    ))),
                    _ => Err(runtime_error(format!("Selection has no property {property}"))),
                }
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
                "target" => Ok(node_value(event.target.clone())),
                "currentTarget" => Ok(node_value(event.current_target.clone())),
                "defaultPrevented" => Ok(Value::Bool(event.state.borrow().default_prevented)),
                "state" => Ok(event.history_state.borrow().clone()),
                "key" => Ok(Value::String(event.key.clone())),
                "shiftKey" => Ok(Value::Bool(event.modifiers.shift)),
                "ctrlKey" => Ok(Value::Bool(event.modifiers.control)),
                "altKey" => Ok(Value::Bool(event.modifiers.alt)),
                "metaKey" => Ok(Value::Bool(event.modifiers.meta)),
                "isComposing" => Ok(Value::Bool(event.is_composing)),
                "data" => Ok(event
                    .data_value
                    .as_ref()
                    .map(|value| (**value).clone())
                    .unwrap_or_else(|| Value::String(event.data.clone()))),
                "message" => Ok(event
                    .error_message
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Undefined)),
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
                "keys" => Ok(Value::Builtin(Builtin::UrlSearchParamsKeys(params))),
                "values" => Ok(Value::Builtin(Builtin::UrlSearchParamsValues(params))),
                "entries" | "@@iterator" => {
                    Ok(Value::Builtin(Builtin::UrlSearchParamsEntries(params)))
                }
                "forEach" => Ok(Value::Builtin(Builtin::UrlSearchParamsForEach(params))),
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
                "keys" => Ok(Value::Builtin(Builtin::FormDataKeys(form_data))),
                "values" => Ok(Value::Builtin(Builtin::FormDataValues(form_data))),
                "entries" | "@@iterator" => {
                    Ok(Value::Builtin(Builtin::FormDataEntries(form_data)))
                }
                "forEach" => Ok(Value::Builtin(Builtin::FormDataForEach(form_data))),
                _ => Err(runtime_error(format!("FormData has no property {property}"))),
            },
            Value::FileList(files) => match property {
                "length" => Ok(Value::Number(files.len() as f64)),
                "item" => Ok(Value::Builtin(Builtin::FileListItem(files))),
                _ => Err(runtime_error(format!("FileList has no property {property}"))),
            },
            Value::Worker(worker) => {
                let handle = worker.clone();
                let worker = worker.borrow();
                match property {
                    "postMessage" => Ok(Value::Builtin(Builtin::WorkerPostMessage(
                        handle.clone(),
                    ))),
                    "terminate" => Ok(Value::Builtin(Builtin::WorkerTerminate(handle))),
                    "addEventListener" => Ok(Value::Builtin(Builtin::WorkerAddEventListener(
                        handle,
                    ))),
                    "onmessage" => Ok(worker.onmessage.clone().unwrap_or(Value::Null)),
                    "onerror" => Ok(worker.onerror.clone().unwrap_or(Value::Null)),
                    "url" => Ok(Value::String(worker.url.to_string())),
                    "name" => Ok(Value::String(worker.name.clone())),
                    _ => Err(runtime_error(format!("Worker has no property {property}"))),
                }
            }
            Value::Blob(blob) => match property {
                "size" => Ok(Value::Number(blob.bytes.len() as f64)),
                "type" => Ok(Value::String(blob.mime_type.clone())),
                "slice" => Ok(Value::Builtin(Builtin::BlobSlice(blob))),
                "text" => Ok(Value::Builtin(Builtin::BlobText(blob))),
                "arrayBuffer" => Ok(Value::Builtin(Builtin::BlobBytes(blob))),
                _ => Err(runtime_error(format!("Blob has no property {property}"))),
            },
            Value::BlobConstructor => Err(runtime_error(format!(
                "Blob has no property {property}"
            ))),
            Value::File(file) => match property {
                "size" => Ok(Value::Number(file.bytes.len() as f64)),
                "type" => Ok(Value::String(file.mime_type.clone())),
                "name" => Ok(Value::String(file.name.clone())),
                "lastModified" => Ok(Value::Number(file.last_modified as f64)),
                "slice" => Ok(Value::Builtin(Builtin::FileSlice(file))),
                "text" => Ok(Value::Builtin(Builtin::FileText(file))),
                "arrayBuffer" => Ok(Value::Builtin(Builtin::FileBytes(file))),
                _ => Err(runtime_error(format!("File has no property {property}"))),
            },
            Value::FileConstructor => Err(runtime_error(format!(
                "File has no property {property}"
            ))),
            Value::CanvasContext(context) => match property {
                "fillStyle" => Ok(Value::String(color_css(context.borrow().fill_style))),
                "fillRect" => Ok(Value::Builtin(Builtin::CanvasFillRect(context))),
                "clearRect" => Ok(Value::Builtin(Builtin::CanvasClearRect(context))),
                _ => Err(runtime_error(format!(
                    "CanvasRenderingContext2D has no property {property}"
                ))),
            },
            Value::AbortController(controller) => match property {
                "signal" => Ok(Value::AbortSignal(controller.borrow().signal.clone())),
                "abort" => Ok(Value::Builtin(Builtin::AbortControllerAbort(controller))),
                _ => Err(runtime_error(format!(
                    "AbortController has no property {property}"
                ))),
            },
            Value::AbortSignal(signal) => match property {
                "aborted" => Ok(Value::Bool(signal.borrow().aborted)),
                "reason" => Ok(signal
                    .borrow()
                    .reason
                    .clone()
                    .unwrap_or(Value::Undefined)),
                "addEventListener" => Ok(Value::Builtin(
                    Builtin::AbortSignalAddEventListener(signal),
                )),
                "throwIfAborted" => Ok(Value::Builtin(
                    Builtin::AbortSignalThrowIfAborted(signal),
                )),
                _ => Err(runtime_error(format!(
                    "AbortSignal has no property {property}"
                ))),
            },
            Value::Response(response) => match property {
                "type" => Ok(Value::String(response.response_type.clone())),
                "url" => Ok(Value::String(response.url.clone())),
                "status" => Ok(Value::Number(response.status as f64)),
                "ok" => Ok(Value::Bool((200..300).contains(&response.status))),
                "redirected" => Ok(Value::Bool(response.redirected)),
                "statusText" => Ok(Value::String(response.status_text.clone())),
                "mimeType" => Ok(Value::String(response.mime_type.clone())),
                "headers" => Ok(Value::Headers(response.headers.clone())),
                "bodyUsed" => Ok(Value::Bool(response_body_used(&response.body))),
                "body" => Ok(Value::ReadableStream(response.stream.clone())),
                "clone" => Ok(Value::Builtin(Builtin::ResponseClone(response))),
                "text" => Ok(Value::Builtin(Builtin::ResponseText(response))),
                "json" => Ok(Value::Builtin(Builtin::ResponseJson(response))),
                "arrayBuffer" => Ok(Value::Builtin(Builtin::ResponseBytes(response))),
                "blob" => Ok(Value::Builtin(Builtin::ResponseBlob(response))),
                "formData" => Ok(Value::Builtin(Builtin::ResponseFormData(response))),
                _ => Err(runtime_error(format!(
                    "response has no property {property}"
                ))),
            },
            Value::ResponseConstructor => match property {
                "error" => Ok(Value::Builtin(Builtin::ResponseErrorConstructor)),
                "json" => Ok(Value::Builtin(Builtin::ResponseJsonConstructor)),
                "redirect" => Ok(Value::Builtin(Builtin::ResponseRedirectConstructor)),
                _ => Err(runtime_error(format!(
                    "Response has no property {property}"
                ))),
            },
            Value::Request(request) => match property {
                "url" => Ok(Value::String(request.url.to_string())),
                "method" => Ok(Value::String(request.method.clone())),
                "headers" => Ok(Value::Headers(request.headers.clone())),
                "body" => Ok(Value::ReadableStream(request.stream.clone())),
                "cache" => Ok(Value::String(fetch_cache_mode_name(&request.cache_mode))),
                "redirect" => Ok(Value::String(fetch_redirect_mode_name(request.redirect))),
                "bodyUsed" => Ok(Value::Bool(response_body_used(&request.body))),
                "signal" => Ok(request
                    .signal
                    .clone()
                    .map(Value::AbortSignal)
                    .unwrap_or(Value::Null)),
                "clone" => Ok(Value::Builtin(Builtin::RequestClone(request))),
                "text" => Ok(Value::Builtin(Builtin::RequestText(request))),
                "arrayBuffer" => Ok(Value::Builtin(Builtin::RequestBytes(request))),
                "blob" => Ok(Value::Builtin(Builtin::RequestBlob(request))),
                "formData" => Ok(Value::Builtin(Builtin::RequestFormData(request))),
                _ => Err(runtime_error(format!(
                    "request has no property {property}"
                ))),
            },
            Value::ReadableStream(stream) => match property {
                "locked" => Ok(Value::Bool(stream.borrow().body.borrow().locked)),
                "getReader" => Ok(Value::Builtin(Builtin::ReadableStreamGetReader(stream))),
                "cancel" => Ok(Value::Builtin(Builtin::ReadableStreamCancel(stream))),
                "tee" => Ok(Value::Builtin(Builtin::ReadableStreamTee(stream))),
                _ => Err(runtime_error(format!(
                    "ReadableStream has no property {property}"
                ))),
            },
            Value::ReadableStreamReader(reader) => match property {
                "read" => Ok(Value::Builtin(Builtin::ReadableStreamReaderRead(reader))),
                "releaseLock" => Ok(Value::Builtin(
                    Builtin::ReadableStreamReaderReleaseLock(reader),
                )),
                "cancel" => Ok(Value::Builtin(Builtin::ReadableStreamReaderCancel(reader))),
                _ => Err(runtime_error(format!(
                    "ReadableStreamDefaultReader has no property {property}"
                ))),
            },
            Value::Bytes(bytes) => match property {
                "byteLength" | "length" => Ok(Value::Number(bytes.len() as f64)),
                _ => Err(runtime_error(format!(
                    "ArrayBuffer has no property {property}"
                ))),
            },
            Value::Headers(headers) => match property {
                "append" => Ok(Value::Builtin(Builtin::HeadersAppend(headers))),
                "delete" => Ok(Value::Builtin(Builtin::HeadersDelete(headers))),
                "get" => Ok(Value::Builtin(Builtin::HeadersGet(headers))),
                "has" => Ok(Value::Builtin(Builtin::HeadersHas(headers))),
                "keys" => Ok(Value::Builtin(Builtin::HeadersKeys(headers))),
                "values" => Ok(Value::Builtin(Builtin::HeadersValues(headers))),
                "entries" => Ok(Value::Builtin(Builtin::HeadersEntries(headers))),
                "forEach" => Ok(Value::Builtin(Builtin::HeadersForEach(headers))),
                "set" => Ok(Value::Builtin(Builtin::HeadersSet(headers))),
                _ => Err(runtime_error(format!(
                    "headers has no property {property}"
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
            Value::WebAssemblyNamespace => match property {
                "instantiate" => Ok(Value::Builtin(Builtin::WebAssemblyInstantiate)),
                _ => Err(runtime_error(format!(
                    "WebAssembly has no property {property}"
                ))),
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
            Value::FileList(files) => {
                let index = index.number()?;
                if !index.is_finite() || index < 0.0 || index.fract() != 0.0 {
                    return Ok(Value::Undefined);
                }
                Ok(files
                    .get(index as usize)
                    .cloned()
                    .map(Value::File)
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
            Value::FormData(form_data) if index.property_key() == "@@iterator" => {
                Ok(Value::Builtin(Builtin::FormDataEntries(form_data)))
            }
            Value::UrlSearchParams(params) if index.property_key() == "@@iterator" => {
                Ok(Value::Builtin(Builtin::UrlSearchParamsEntries(params)))
            }
            Value::Bytes(bytes) => {
                let index = index.number()?;
                if !index.is_finite() || index < 0.0 || index.fract() != 0.0 {
                    return Ok(Value::Undefined);
                }
                Ok(bytes
                    .get(index as usize)
                    .copied()
                    .map(|byte| Value::Number(byte as f64))
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
        let value_string = value.string_value();
        let updated = match object.resolve() {
            Value::Element(handle) => match property {
                "textContent" => self.set_element_text_content(&handle, value_string),
                "className" => self.set_element_attribute(&handle, "class", value_string),
                "id" => self.set_element_attribute(&handle, "id", value_string),
                "innerHTML" => self.set_element_inner_html(&handle, value_string)?,
                "value" => self.set_element_value(&handle, value_string),
                "selectionStart" => self.set_element_selection_start(&handle, value.number()?)?,
                "selectionEnd" => self.set_element_selection_end(&handle, value.number()?)?,
                "selectionDirection" => {
                    self.set_element_selection_direction(&handle, &value_string)?
                }
                "checked" => self.set_element_checked(&handle, truthy),
                "disabled" => self.set_element_disabled(&handle, truthy),
                "readOnly" => self.set_element_readonly(&handle, truthy),
                "required" => self.set_element_required(&handle, truthy),
                "maxLength" => self
                    .set_element_length_attribute(&handle, "maxlength", value.number()?)?,
                "minLength" => self
                    .set_element_length_attribute(&handle, "minlength", value.number()?)?,
                "scrollLeft" => self.set_scroll_offset_property(&handle, 0, value.number()?)?,
                "scrollTop" => self.set_scroll_offset_property(&handle, 1, value.number()?)?,
                _ => false,
            },
            Value::Dataset(handle) => {
                self.set_element_attribute(&handle, &dataset_attribute_name(property), value_string)
            }
            Value::Style(handle) => self.set_element_style_property(&handle, property, value_string),
            Value::CanvasContext(context) if property == "fillStyle" => {
                self.set_canvas_fill_style(&context, &value_string)
            }
            Value::Window if property == "location" => {
                self.request_location_navigation(value_string, false);
                true
            }
            Value::Window if self.worker_sender.is_some() && matches!(property, "onmessage" | "onerror") => {
                self.assign_worker_global_callback(property, value)?;
                true
            }
            Value::Location(_) if property == "href" => {
                self.request_location_navigation(value_string, false);
                true
            }
            Value::Url(url) => self.assign_url_property(&url, property, &value_string)?,
            Value::Worker(worker) if matches!(property, "onmessage" | "onerror") => {
                self.assign_worker_callback(&worker, property, value)?;
                true
            }
            _ => false,
        };
        updated
            .then_some(())
            .ok_or_else(|| runtime_error(format!("property {property} is not writable")))
    }

    fn assign_worker_global_callback(
        &mut self,
        property: &str,
        value: Value,
    ) -> Result<(), ScriptError> {
        let callback = match value.resolve() {
            Value::Function(_) => value,
            Value::Null | Value::Undefined => Value::Null,
            _ => return Err(runtime_error(format!("Worker {property} must be a function or null"))),
        };
        self.global
            .borrow_mut()
            .values
            .insert(property.to_string(), callback);
        Ok(())
    }

    fn assign_worker_callback(
        &mut self,
        worker: &WorkerHandle,
        property: &str,
        value: Value,
    ) -> Result<(), ScriptError> {
        let callback = match value.resolve() {
            Value::Function(_) => Some(value),
            Value::Null | Value::Undefined => None,
            _ => return Err(runtime_error(format!("Worker {property} must be a function or null"))),
        };
        let mut worker = worker.borrow_mut();
        match property {
            "onmessage" => worker.onmessage = callback,
            "onerror" => worker.onerror = callback,
            _ => return Err(runtime_error(format!("Worker has no writable property {property}"))),
        }
        Ok(())
    }

    fn construct_worker(&mut self, arguments: Vec<Value>) -> Result<Value, ScriptError> {
        let reference = arguments
            .first()
            .ok_or_else(|| runtime_error("Worker requires a script URL"))?
            .string_value();
        let base_url = self
            .base_url
            .clone()
            .unwrap_or_else(|| self.location.borrow().clone());
        let url = base_url
            .resolve(&reference)
            .map_err(|error| runtime_error(error.to_string()))?;
        let mut name = String::new();
        let mut worker_type = WorkerScriptType::Classic;
        if let Some(options) = arguments.get(1).map(Value::resolve) {
            match options {
                Value::Undefined | Value::Null => {}
                Value::Object(options) => {
                    for property in options.borrow().keys() {
                        if !matches!(property.as_str(), "name" | "type") {
                            return Err(runtime_error(format!(
                                "Worker option {property} is not supported"
                            )));
                        }
                    }
                    if let Some(type_value) = options.borrow().get("type") {
                        let type_name = type_value.string_value();
                        worker_type = match type_name.to_ascii_lowercase().as_str() {
                            "classic" => WorkerScriptType::Classic,
                            "module" => WorkerScriptType::Module,
                            _ => {
                                return Err(runtime_error(format!(
                                    "unsupported Worker script type {type_name}"
                                )))
                            }
                        };
                    }
                    if let Some(worker_name) = options.borrow().get("name") {
                        name = worker_name.string_value();
                    }
                }
                _ => return Err(runtime_error("Worker options must be an object")),
            }
        }
        let resources = self.resources.clone().unwrap_or_default();
        let provider = self.resource_provider.clone();
        let resource = resolve_runtime_resource(&resources, provider.as_deref(), &url)?
            .ok_or_else(|| runtime_error(format!("worker script {} was not found", url.path())))?;
        if !is_javascript(&resource) {
            return Err(runtime_error(format!(
                "worker script {} has unsupported MIME type {}",
                url.path(),
                resource.mime_type()
            )));
        }
        if resource.body().len() > MAX_WORKER_SOURCE_BYTES {
            return Err(runtime_error("worker script exceeds the 1 MiB limit"));
        }
        let source = String::from_utf8(resource.body().to_vec())
            .map_err(|_| runtime_error(format!("worker script {} is not UTF-8", url.path())))?;
        let (command_sender, command_receiver) = mpsc::sync_channel(MAX_WORKER_MESSAGES);
        let (event_sender, event_receiver) = mpsc::sync_channel(MAX_WORKER_MESSAGES);
        let termination_flag = Arc::new(AtomicBool::new(false));
        let worker_termination_flag = termination_flag.clone();
        let worker_url = url.clone();
        let worker_resource_sender = if self.resource_provider.is_some() {
            match self.worker_resource_sender.clone() {
                Some(sender) => Some(sender),
                None => {
                    let (sender, receiver) = mpsc::sync_channel(MAX_WORKER_MESSAGES);
                    self.worker_resource_sender = Some(sender.clone());
                    self.worker_resource_receiver = Some(receiver);
                    Some(sender)
                }
            }
        } else {
            None
        };
        thread::spawn(move || {
            let document = match Document::parse("<html><body></body></html>") {
                Ok(document) => Rc::new(RefCell::new(document)),
                Err(error) => {
                    let _ = event_sender.try_send(WorkerEvent::Error(error.to_string()));
                    return;
                }
            };
            let worker_provider = worker_resource_sender.map(|sender| {
                Rc::new(WorkerResourceProvider { sender }) as Rc<dyn ResourceProvider>
            });
            let mut runtime = ScriptRuntime::with_context(
                document,
                Some(resources),
                worker_provider,
                Some(worker_url.clone()),
                None,
                None,
                None,
            );
            runtime.worker_sender = Some(event_sender.clone());
            runtime.worker_script_type = Some(worker_type);
            runtime
                .global
                .borrow_mut()
                .values
                .insert("self".to_string(), Value::Window);
            runtime
                .global
                .borrow_mut()
                .values
                .insert(
                    "postMessage".to_string(),
                    Value::Builtin(Builtin::WorkerGlobalPostMessage(event_sender.clone())),
                );
            runtime
                .global
                .borrow_mut()
                .values
                .insert("globalThis".to_string(), Value::Window);
            if matches!(worker_type, WorkerScriptType::Classic) {
                runtime.global.borrow_mut().values.insert(
                    "importScripts".to_string(),
                    Value::Builtin(Builtin::WorkerGlobalImportScripts),
                );
            }
            let startup = match worker_type {
                WorkerScriptType::Classic => runtime.execute(&source),
                WorkerScriptType::Module => {
                    let loader_resources = runtime.resources.clone().unwrap_or_default();
                    let loader_provider = runtime.resource_provider.clone();
                    let mut loader = RuntimeModuleLoader {
                        resources: &loader_resources,
                        provider: loader_provider.as_deref(),
                    };
                    runtime
                        .execute_module(&source, worker_url.clone(), &mut loader)
                        .map(|_| ())
                }
            };
            if let Err(error) = startup {
                let _ = event_sender.try_send(WorkerEvent::Error(error.message));
                return;
            }
            while !worker_termination_flag.load(Ordering::Acquire) {
                let Ok(command) = command_receiver.recv() else {
                    break;
                };
                match command {
                    WorkerCommand::Terminate => break,
                    WorkerCommand::Message(message) => {
                        if worker_termination_flag.load(Ordering::Acquire) {
                            break;
                        }
                        let data = match runtime.parse_json(&message) {
                            Ok(data) => data,
                            Err(error) => {
                                let _ = event_sender.try_send(WorkerEvent::Error(error.message));
                                continue;
                            }
                        };
                        let mut callbacks = runtime
                            .window_listeners
                            .get("message")
                            .cloned()
                            .unwrap_or_default();
                        if let Some(callback) = runtime.global.borrow().get("onmessage")
                            && matches!(callback.resolve(), Value::Function(_))
                        {
                            callbacks.push(callback);
                        }
                        if callbacks.is_empty() {
                            continue;
                        }
                        let event = Value::Event(EventData {
                            event_type: "message".to_string(),
                            target: Vec::new(),
                            current_target: Vec::new(),
                            state: Rc::new(RefCell::new(EventState::default())),
                            history_state: Rc::new(RefCell::new(Value::Null)),
                            key: String::new(),
                            data: String::new(),
                            modifiers: KeyModifiers::default(),
                            is_composing: false,
                            data_value: Some(Box::new(data)),
                            error_message: None,
                        });
                        let mut callback_failed = false;
                        for callback in callbacks {
                            if let Err(error) = runtime.call(callback, vec![event.clone()]) {
                                let _ = event_sender.try_send(WorkerEvent::Error(error.message));
                                callback_failed = true;
                                break;
                            }
                        }
                        if callback_failed {
                            continue;
                        }
                        if let Err(error) = runtime.drain_microtasks() {
                            let _ = event_sender.try_send(WorkerEvent::Error(error.message));
                        }
                    }
                }
            }
        });
        let worker = Rc::new(RefCell::new(WorkerData {
            url,
            name,
            command_sender,
            event_receiver,
            onmessage: None,
            onerror: None,
            listeners: Vec::new(),
            terminated: false,
            termination_flag,
        }));
        self.workers.push(worker.clone());
        Ok(Value::Worker(worker))
    }

    pub fn pump(&mut self) -> Result<usize, ScriptError> {
        let processed = self.pump_workers()?;
        self.drain_microtasks()?;
        Ok(processed)
    }

    fn pump_worker_resource_requests(&mut self) -> Result<(), ScriptError> {
        let Some(receiver) = self.worker_resource_receiver.as_ref() else {
            return Ok(());
        };
        loop {
            let request = match receiver.try_recv() {
                Ok(request) => request,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            };
            let response = self
                .resource_provider
                .as_ref()
                .map_or(Ok(None), |provider| provider.provide_response(&request.request));
            let _ = request.reply.send(response);
        }
        Ok(())
    }

    fn pump_workers(&mut self) -> Result<usize, ScriptError> {
        self.pump_worker_resource_requests()?;
        let workers = self.workers.clone();
        let mut processed = 0;
        for worker in workers {
            loop {
                let event = match worker.borrow_mut().event_receiver.try_recv() {
                    Ok(event) => event,
                    Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                };
                processed += 1;
                match event {
                    WorkerEvent::Message(message) => {
                        let data = self.parse_json(&message)?;
                        let worker_data = worker.borrow();
                        let mut callbacks = worker_data
                            .listeners
                            .iter()
                            .filter(|listener| listener.event == "message")
                            .map(|listener| listener.callback.clone())
                            .collect::<Vec<_>>();
                        if let Some(callback) = worker_data.onmessage.clone() {
                            callbacks.push(callback);
                        }
                        drop(worker_data);
                        if callbacks.is_empty() {
                            continue;
                        }
                        let event = Value::Event(EventData {
                            event_type: "message".to_string(),
                            target: Vec::new(),
                            current_target: Vec::new(),
                            state: Rc::new(RefCell::new(EventState::default())),
                            history_state: Rc::new(RefCell::new(Value::Null)),
                            key: String::new(),
                            data: String::new(),
                            modifiers: KeyModifiers::default(),
                            is_composing: false,
                            data_value: Some(Box::new(data)),
                            error_message: None,
                        });
                        for callback in callbacks {
                            self.call(callback, vec![event.clone()])?;
                        }
                    }
                    WorkerEvent::Error(message) => {
                        let worker_data = worker.borrow();
                        let mut callbacks = worker_data
                            .listeners
                            .iter()
                            .filter(|listener| listener.event == "error")
                            .map(|listener| listener.callback.clone())
                            .collect::<Vec<_>>();
                        if let Some(callback) = worker_data.onerror.clone() {
                            callbacks.push(callback);
                        }
                        drop(worker_data);
                        if callbacks.is_empty() {
                            continue;
                        }
                        let event = Value::Event(EventData {
                            event_type: "error".to_string(),
                            target: Vec::new(),
                            current_target: Vec::new(),
                            state: Rc::new(RefCell::new(EventState::default())),
                            history_state: Rc::new(RefCell::new(Value::Null)),
                            key: String::new(),
                            data: String::new(),
                            modifiers: KeyModifiers::default(),
                            is_composing: false,
                            data_value: None,
                            error_message: Some(message),
                        });
                        for callback in callbacks {
                            self.call(callback, vec![event.clone()])?;
                        }
                    }
                }
            }
        }
        self.pump_worker_resource_requests()?;
        Ok(processed)
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

    fn is_readonly_path(&self, path: &[usize]) -> bool {
        self.is_selection_control_path(path)
            && self
                .document
                .borrow()
                .element_clone(path)
                .is_some_and(|element| element.attr("readonly").is_some())
    }

    fn user_input_max_length(&self, path: &[usize]) -> Option<usize> {
        if !self.is_selection_control_path(path) {
            return None;
        }
        self.control_length_attribute(path, "maxlength")
    }

    fn control_length_attribute(&self, path: &[usize], name: &str) -> Option<usize> {
        self.document
            .borrow()
            .element_clone(path)
            .and_then(|element| element.attr(name).and_then(|value| value.trim().parse().ok()))
    }

    fn mark_user_edit(&mut self, path: &[usize]) {
        self.dirty_controls.insert(path.to_vec());
        self.last_user_edit.insert(path.to_vec());
    }

    fn is_validation_candidate(&self, path: &[usize]) -> bool {
        let Some(element) = self.document.borrow().element_clone(path) else {
            return false;
        };
        if element.attr("disabled").is_some() {
            return false;
        }
        match element.name.as_str() {
            "textarea" => !self.is_readonly_path(path),
            "input" => {
                let kind = element
                    .attr("type")
                    .map(|kind| kind.to_ascii_lowercase())
                    .unwrap_or_else(|| "text".to_string());
                !(matches!(kind.as_str(), "button" | "hidden" | "image" | "reset" | "submit")
                    || element.attr("readonly").is_some()
                        && self.is_selection_control_path(path))
            }
            _ => false,
        }
    }

    fn constraint_flags(&self, path: &[usize]) -> ConstraintFlags {
        if !self.is_validation_candidate(path) {
            return ConstraintFlags::default();
        }
        let Some(element) = self.document.borrow().element_clone(path) else {
            return ConstraintFlags::default();
        };
        let kind = element
            .attr("type")
            .map(|kind| kind.to_ascii_lowercase())
            .unwrap_or_else(|| "text".to_string());
        let value = self.control_value_path(path).unwrap_or_default();
        let empty = match kind.as_str() {
            "checkbox" => !self.control_checked_path(path),
            "radio" => self
                .document
                .borrow()
                .radio_group_paths(path)
                .map(|group| {
                    group
                        .iter()
                        .all(|candidate| !self.control_checked_path(candidate))
                })
                .unwrap_or_else(|| !self.control_checked_path(path)),
            "file" => self
                .selected_files
                .get(path)
                .is_none_or(Vec::is_empty),
            _ => value.is_empty(),
        };
        let dirty = self.dirty_controls.contains(path);
        let length = utf16_len(&value);
        let too_long = dirty
            && self.last_user_edit.contains(path)
            && self.is_selection_control_path(path)
            && self
                .control_length_attribute(path, "maxlength")
                .is_some_and(|maximum| length > maximum);
        let too_short = dirty
            && self.last_user_edit.contains(path)
            && !empty
            && self.is_selection_control_path(path)
            && self
                .control_length_attribute(path, "minlength")
                .is_some_and(|minimum| length < minimum);
        let value_missing = element.attr("required").is_some() && empty;
        let type_mismatch = !empty
            && match kind.as_str() {
                "email" => !valid_email_value(&value, element.attr("multiple").is_some()),
                "url" => !valid_url_value(&value),
                _ => false,
            };
        let pattern_mismatch = !empty
            && element
                .attr("pattern")
                .and_then(|pattern| pattern_matches(pattern, &value))
                .is_some_and(|matches| !matches);
        ConstraintFlags {
            value_missing,
            type_mismatch,
            pattern_mismatch,
            too_long,
            too_short,
            custom_error: self
                .custom_validity
                .get(path)
                .is_some_and(|message| !message.is_empty()),
            ..ConstraintFlags::default()
        }
    }

    fn element_validity(&self, handle: &ElementHandle) -> Value {
        let Some(path) = attached_path(handle) else {
            return validity_object(ConstraintFlags::default());
        };
        Value::Validity(Rc::new(path))
    }

    fn validation_message(&self, path: &[usize]) -> String {
        let flags = self.constraint_flags(path);
        if flags.custom_error {
            let message = self
                .custom_validity
                .get(path)
                .expect("custom validity flag has a message");
            return message.clone();
        }
        if flags.value_missing {
            return "value missing".to_string();
        }
        if flags.type_mismatch {
            return "type mismatch".to_string();
        }
        if flags.pattern_mismatch {
            return "pattern mismatch".to_string();
        }
        if flags.too_long {
            return "value too long".to_string();
        }
        if flags.too_short {
            return "value too short".to_string();
        }
        String::new()
    }

    fn check_validity_path(&mut self, path: &[usize]) -> Result<bool, ScriptError> {
        let Some(element) = self.document.borrow().element_clone(path) else {
            return Ok(true);
        };
        if element.name == "form" {
            let controls = self
                .document
                .borrow()
                .form_control_paths(path)
                .unwrap_or_default();
            let mut valid = true;
            for control in controls {
                if !self.check_validity_path(&control)? {
                    valid = false;
                }
            }
            return Ok(valid);
        }
        if !self.is_validation_candidate(path) {
            return Ok(true);
        }
        let flags = self.constraint_flags(path);
        if flags.valid() {
            return Ok(true);
        }
        self.dispatch_non_bubbling_event(path.to_vec(), "invalid")?;
        self.drain_microtasks()?;
        Ok(false)
    }

    fn set_custom_validity(&mut self, path: NodePath, message: String) {
        if message.is_empty() {
            self.custom_validity.remove(&path);
        } else {
            self.custom_validity.insert(path, message);
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

    fn is_selection_control_path(&self, path: &[usize]) -> bool {
        let Some(element) = self.document.borrow().element_clone(path) else {
            return false;
        };
        match element.name.as_str() {
            "textarea" => true,
            "input" => element.attr("type").is_none_or(|kind| {
                !matches!(
                    kind.to_ascii_lowercase().as_str(),
                    "button"
                        | "checkbox"
                        | "color"
                        | "date"
                        | "datetime-local"
                        | "file"
                        | "hidden"
                        | "image"
                        | "month"
                        | "number"
                        | "radio"
                        | "range"
                        | "reset"
                        | "submit"
                        | "time"
                        | "week"
                )
            }),
            _ => false,
        }
    }

    fn selection_for_path(&self, path: &[usize], value: &str) -> TextSelection {
        let length = utf16_len(value);
        let selection = self.text_selections.get(path).copied().unwrap_or_default();
        TextSelection {
            start: selection.start.min(length),
            end: selection.end.min(length),
            direction: selection.direction,
        }
    }

    fn element_selection_start(&self, handle: &ElementHandle) -> Result<Value, ScriptError> {
        let Some(path) = attached_path(handle) else {
            return Ok(Value::Null);
        };
        if !self.is_selection_control_path(&path) {
            return Ok(Value::Null);
        }
        let value = self.control_value_path(&path).unwrap_or_default();
        Ok(Value::Number(
            self.selection_for_path(&path, &value).start as f64,
        ))
    }

    fn element_selection_end(&self, handle: &ElementHandle) -> Result<Value, ScriptError> {
        let Some(path) = attached_path(handle) else {
            return Ok(Value::Null);
        };
        if !self.is_selection_control_path(&path) {
            return Ok(Value::Null);
        }
        let value = self.control_value_path(&path).unwrap_or_default();
        Ok(Value::Number(
            self.selection_for_path(&path, &value).end as f64,
        ))
    }

    fn element_selection_direction(&self, handle: &ElementHandle) -> Result<Value, ScriptError> {
        let Some(path) = attached_path(handle) else {
            return Ok(Value::Null);
        };
        if !self.is_selection_control_path(&path) {
            return Ok(Value::Null);
        }
        let value = self.control_value_path(&path).unwrap_or_default();
        Ok(Value::String(
            selection_direction_name(self.selection_for_path(&path, &value).direction).to_string(),
        ))
    }

    fn set_element_selection_start(
        &mut self,
        handle: &ElementHandle,
        start: f64,
    ) -> Result<bool, ScriptError> {
        let Some(path) = attached_path(handle) else {
            return Ok(false);
        };
        if !self.is_selection_control_path(&path) {
            return Ok(false);
        }
        let value = self.control_value_path(&path).unwrap_or_default();
        let selection = self.selection_for_path(&path, &value);
        let start = selection_offset(start);
        self.set_text_selection(
            path,
            start,
            selection.end.max(start),
            selection.direction,
        )
    }

    fn set_element_selection_end(
        &mut self,
        handle: &ElementHandle,
        end: f64,
    ) -> Result<bool, ScriptError> {
        let Some(path) = attached_path(handle) else {
            return Ok(false);
        };
        if !self.is_selection_control_path(&path) {
            return Ok(false);
        }
        let value = self.control_value_path(&path).unwrap_or_default();
        let selection = self.selection_for_path(&path, &value);
        self.set_text_selection(
            path,
            selection.start,
            selection_offset(end),
            selection.direction,
        )
    }

    fn set_element_selection_direction(
        &mut self,
        handle: &ElementHandle,
        direction: &str,
    ) -> Result<bool, ScriptError> {
        let Some(path) = attached_path(handle) else {
            return Ok(false);
        };
        if !self.is_selection_control_path(&path) {
            return Ok(false);
        }
        let value = self.control_value_path(&path).unwrap_or_default();
        let selection = self.selection_for_path(&path, &value);
        self.set_text_selection(
            path,
            selection.start,
            selection.end,
            selection_direction(direction),
        )
    }

    fn set_text_selection(
        &mut self,
        path: NodePath,
        start: usize,
        end: usize,
        direction: SelectionDirection,
    ) -> Result<bool, ScriptError> {
        if !self.is_selection_control_path(&path) {
            return Ok(false);
        }
        let value = self.control_value_path(&path).unwrap_or_default();
        let length = utf16_len(&value);
        let start = start.min(length);
        let end = end.min(length);
        let (start, end) = if end <= start {
            (end, end)
        } else {
            (start, end)
        };
        let next = TextSelection {
            start,
            end,
            direction,
        };
        let previous = self.text_selections.get(&path).copied().unwrap_or_default();
        if previous == next {
            return Ok(false);
        }
        self.text_selections.insert(path.clone(), next);
        self.dispatch_event(path.clone(), "select")?;
        self.dispatch_event(path, "selectionchange")?;
        self.drain_microtasks()?;
        Ok(true)
    }

    fn set_range_text(&mut self, path: &NodePath, arguments: &[Value]) -> Result<(), ScriptError> {
        if !self.is_selection_control_path(path) {
            return Ok(());
        }
        let value = self.control_value_path(path).unwrap_or_default();
        let units = value.encode_utf16().collect::<Vec<_>>();
        let current = self.selection_for_path(path, &value);
        let has_range = arguments.len() >= 3;
        let replacement = arguments
            .first()
            .map(Value::string_value)
            .unwrap_or_default();
        let replacement_units = replacement.encode_utf16().collect::<Vec<_>>();
        let (start, end) = if has_range {
            let start = selection_offset(arguments[1].number()?);
            let end = selection_offset(arguments[2].number()?);
            if start > end {
                return Err(runtime_error("setRangeText start exceeds end"));
            }
            (start.min(units.len()), end.min(units.len()))
        } else {
            (current.start, current.end)
        };
        let mode = arguments
            .get(3)
            .map(Value::string_value)
            .unwrap_or_else(|| "preserve".to_string());
        if !matches!(mode.as_str(), "select" | "start" | "end" | "preserve") {
            return Err(runtime_error(
                "setRangeText selection mode must be select, start, end, or preserve",
            ));
        }
        let mut next_units = Vec::with_capacity(
            units
                .len()
                .saturating_sub(end.saturating_sub(start))
                .saturating_add(replacement_units.len()),
        );
        next_units.extend_from_slice(&units[..start]);
        next_units.extend_from_slice(&replacement_units);
        next_units.extend_from_slice(&units[end..]);
        self.control_values
            .insert(path.clone(), String::from_utf16_lossy(&next_units));

        let new_end = start.saturating_add(replacement_units.len());
        let (selection_start, selection_end) = match mode.as_str() {
            "select" => (start, new_end),
            "start" => (start, start),
            "end" => (new_end, new_end),
            "preserve" if has_range => (current.start, current.end),
            "preserve" => (
                preserve_selection_offset(current.start, start, end, new_end),
                preserve_selection_offset(current.end, start, end, new_end),
            ),
            _ => unreachable!("setRangeText mode was validated above"),
        };
        self.set_text_selection(
            path.clone(),
            selection_start,
            selection_end,
            SelectionDirection::None,
        )?;
        self.dispatch_event_with_details(path.clone(), "input", "", &replacement)?;
        self.drain_microtasks()?;
        Ok(())
    }

    fn range_boundary_from_arguments(
        &self,
        arguments: &[Value],
    ) -> Result<RangeBoundary, ScriptError> {
        let Some(Value::Element(handle)) = arguments.first().map(Value::resolve) else {
            return Err(runtime_error("Range boundary requires an Element"));
        };
        let Some(path) = attached_path(&handle) else {
            return Err(runtime_error("Range boundary requires an attached Element"));
        };
        let value = self
            .document
            .borrow()
            .element_text_content(&path)
            .ok_or_else(|| runtime_error("Range boundary element no longer exists"))?;
        let offset = arguments
            .get(1)
            .map(Value::number)
            .transpose()?
            .map(selection_offset)
            .unwrap_or_default()
            .min(utf16_len(&value));
        Ok(RangeBoundary { path, offset })
    }

    fn range_to_string(&self, range: &RangeHandle) -> String {
        let range = range.borrow();
        if range.start.path != range.end.path {
            return String::new();
        }
        let value = self
            .document
            .borrow()
            .element_text_content(&range.start.path)
            .unwrap_or_default();
        utf16_substring(&value, range.start.offset, range.end.offset)
    }

    fn element_files(&self, handle: &ElementHandle) -> Value {
        let Some(path) = attached_path(handle) else {
            return Value::Null;
        };
        let is_file = self
            .document
            .borrow()
            .element_clone(&path)
            .is_some_and(|element| {
                element.name == "input"
                    && element
                        .attr("type")
                        .is_some_and(|kind| kind.eq_ignore_ascii_case("file"))
            });
        if !is_file {
            return Value::Null;
        }
        let mut file_lists = self.file_lists.borrow_mut();
        let files = file_lists
            .entry(path.clone())
            .or_insert_with(|| {
                Rc::new(
                    self.selected_files
                        .get(&path)
                        .cloned()
                        .unwrap_or_default(),
                )
            })
            .clone();
        Value::FileList(files)
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
                let is_file = self
                    .document
                    .borrow()
                    .element_clone(&path)
                    .is_some_and(|element| {
                        element.name == "input"
                            && element
                                .attr("type")
                                .is_some_and(|kind| kind.eq_ignore_ascii_case("file"))
                    });
                if is_file {
                    if !value.is_empty() {
                        return false;
                    }
                    self.selected_files.remove(&path);
                    self.file_lists
                        .borrow_mut()
                        .insert(path.clone(), Rc::new(Vec::new()));
                }
                let value_length = utf16_len(&value);
                self.control_values.insert(path.clone(), value);
                self.dirty_controls.insert(path.clone());
                self.last_user_edit.remove(&path);
                if self.is_selection_control_path(&path)
                    && let Some(selection) = self.text_selections.get_mut(&path)
                {
                    selection.start = selection.start.min(value_length);
                    selection.end = selection.end.min(value_length).max(selection.start);
                }
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
            Value::BlobConstructor => self.construct_blob(arguments),
            Value::FileConstructor => self.construct_file(arguments),
            Value::HeadersConstructor => self.construct_headers(arguments),
            Value::RequestConstructor => self.construct_request(arguments),
            Value::ResponseConstructor => self.construct_response(arguments),
            Value::MapConstructor => self.construct_map(arguments),
            Value::SetConstructor => self.construct_set(arguments),
            Value::ErrorConstructor => Ok(self.construct_error(arguments)),
            Value::PromiseConstructor => self.construct_promise(arguments),
            Value::AbortControllerConstructor => Ok(self.construct_abort_controller()),
            Value::WorkerConstructor => self.construct_worker(arguments),
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

    fn set_element_readonly(&mut self, handle: &ElementHandle, readonly: bool) -> bool {
        match handle.borrow().clone() {
            ElementLocation::Attached(path) => {
                if readonly {
                    self.document
                        .borrow_mut()
                        .set_element_attribute(&path, "readonly", String::new())
                } else {
                    self.document
                        .borrow_mut()
                        .remove_element_attribute(&path, "readonly")
                }
            }
            ElementLocation::Detached(_) => {
                let mut location = handle.borrow_mut();
                let ElementLocation::Detached(element) = &mut *location else {
                    return false;
                };
                if readonly {
                    element.attrs.insert("readonly".to_string(), String::new());
                } else {
                    element.attrs.remove("readonly");
                }
                true
            }
        }
    }

    fn set_element_required(&mut self, handle: &ElementHandle, required: bool) -> bool {
        match handle.borrow().clone() {
            ElementLocation::Attached(path) => {
                if required {
                    self.document
                        .borrow_mut()
                        .set_element_attribute(&path, "required", String::new())
                } else {
                    self.document
                        .borrow_mut()
                        .remove_element_attribute(&path, "required")
                }
            }
            ElementLocation::Detached(_) => {
                let mut location = handle.borrow_mut();
                let ElementLocation::Detached(element) = &mut *location else {
                    return false;
                };
                if required {
                    element.attrs.insert("required".to_string(), String::new());
                } else {
                    element.attrs.remove("required");
                }
                true
            }
        }
    }

    fn set_element_length_attribute(
        &mut self,
        handle: &ElementHandle,
        name: &str,
        value: f64,
    ) -> Result<bool, ScriptError> {
        if !value.is_finite()
            || value.fract() != 0.0
            || value < 0.0
            || value > usize::MAX as f64
        {
            return Err(runtime_error(format!("{name} must be a non-negative integer")));
        }
        let value = value as usize;
        Ok(self.set_element_attribute(handle, name, value.to_string()))
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
                if input_type == "file" {
                    let files = self
                        .selected_files
                        .get(&path)
                        .cloned()
                        .unwrap_or_default();
                    if files.is_empty() {
                        pairs.push((
                            name.to_string(),
                            FormDataEntry::File(Rc::new(FileData {
                                bytes: Vec::new(),
                                mime_type: "application/octet-stream".to_string(),
                                name: String::new(),
                                last_modified: 0,
                            })),
                        ));
                    } else {
                        for file in files {
                            pairs.push((name.to_string(), FormDataEntry::File(file)));
                        }
                    }
                }
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
            pairs.push((name.to_string(), FormDataEntry::Text(value)));
        }
        Ok(Value::FormData(Rc::new(RefCell::new(FormDataData { pairs }))))
    }

    fn construct_blob(&self, arguments: Vec<Value>) -> Result<Value, ScriptError> {
        let bytes = blob_parts_bytes(arguments.first())?;
        let (mime_type, _) = blob_options(arguments.get(1), false)?;
        Ok(Value::Blob(Rc::new(BlobData { bytes, mime_type })))
    }

    fn construct_file(&self, arguments: Vec<Value>) -> Result<Value, ScriptError> {
        let bytes = blob_parts_bytes(arguments.first())?;
        let name = arguments
            .get(1)
            .map(Value::string_value)
            .ok_or_else(|| runtime_error("File constructor requires a name"))?;
        let (mime_type, last_modified) = blob_options(arguments.get(2), true)?;
        Ok(Value::File(Rc::new(FileData {
            bytes,
            mime_type,
            name: normalize_file_name(&name),
            last_modified,
        })))
    }

    fn construct_headers(&self, arguments: Vec<Value>) -> Result<Value, ScriptError> {
        let init = arguments
            .first()
            .cloned()
            .unwrap_or(Value::Undefined)
            .resolve();
        Ok(Value::Headers(headers_handle(header_values_from_init(init)?, true)))
    }

    fn construct_request(&self, arguments: Vec<Value>) -> Result<Value, ScriptError> {
        let input = arguments
            .first()
            .cloned()
            .unwrap_or(Value::Undefined)
            .resolve();
        let (
            url,
            mut method,
            mut headers,
            mut body,
            mut cache_mode,
            mut redirect,
            mut signal,
        ) = match input {
            Value::Request(request) => (
                request.url.clone(),
                request.method.clone(),
                request.headers.borrow().values.clone(),
                request_body_snapshot(&request.body)?,
                request.cache_mode.clone(),
                request.redirect,
                request.signal.clone(),
            ),
            input => {
                let base_url = self
                    .base_url
                    .as_ref()
                    .ok_or_else(|| runtime_error("Request is unavailable without a document URL"))?;
                let url = base_url
                    .resolve(&input.string_value())
                    .map_err(|error| runtime_error(error.to_string()))?;
                (
                    url,
                    "GET".to_string(),
                    BTreeMap::new(),
                    Vec::new(),
                    ResourceCacheMode::Default,
                    FetchRedirectMode::Follow,
                    None,
                )
            }
        };
        let init = arguments
            .get(1)
            .cloned()
            .unwrap_or(Value::Undefined)
            .resolve();
        let init = match init {
            Value::Undefined | Value::Null => None,
            Value::Object(init) => Some(init),
            _ => return Err(runtime_error("Request init must be an object")),
        };
        if let Some(init) = init {
            let init = init.borrow();
            if init.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "method" | "headers" | "body" | "cache" | "redirect" | "signal"
                )
            }) {
                return Err(runtime_error("Request init contains an unsupported key"));
            }
            if let Some(value) = init.get("method").map(Value::resolve) {
                method = match value {
                    Value::Undefined => method,
                    Value::String(method) => method,
                    _ => return Err(runtime_error("Request method must be a string")),
                };
            }
            if let Some(value) = init.get("headers").map(Value::resolve) {
                headers = match value {
                    Value::Undefined => headers,
                    Value::Null => BTreeMap::new(),
                    value => header_values_from_init(value)?,
                };
            }
            if let Some(value) = init.get("body").map(Value::resolve) {
                body = match value {
                    Value::Undefined => body,
                    value => {
                        let (body, inferred_mime_type) = body_init_bytes(value)?;
                        if let Some(mime_type) = inferred_mime_type
                            && !headers.contains_key("content-type")
                        {
                            headers.insert("content-type".to_string(), mime_type);
                        }
                        body
                    }
                };
            }
            cache_mode = parse_cache_mode(init.get("cache"), cache_mode)?;
            redirect = parse_redirect_mode(init.get("redirect"), redirect)?;
            signal = parse_signal(init.get("signal"), signal)?;
        }
        let normalized = ResourceRequest::new(url.clone(), method, headers, body)
            .map_err(|error| runtime_error(error.to_string()))?;
        Ok(Value::Request(request_handle(
            url,
            normalized.method().to_string(),
            normalized.headers().clone(),
            normalized.body().to_vec(),
            cache_mode,
            redirect,
            signal,
        )))
    }

    fn construct_response(&self, arguments: Vec<Value>) -> Result<Value, ScriptError> {
        let body = arguments
            .first()
            .cloned()
            .unwrap_or(Value::Null)
            .resolve();
        let init = arguments
            .get(1)
            .cloned()
            .unwrap_or(Value::Undefined)
            .resolve();
        self.construct_response_parts(body, init, None)
    }

    fn construct_response_parts(
        &self,
        body: Value,
        init: Value,
        inferred_mime_type: Option<&str>,
    ) -> Result<Value, ScriptError> {
        let mut status = 200;
        let mut status_text = String::new();
        let mut headers = BTreeMap::new();
        let init = match init {
            Value::Undefined | Value::Null => None,
            Value::Object(init) => Some(init),
            _ => return Err(runtime_error("Response init must be an object")),
        };
        if let Some(init) = init {
            let init = init.borrow();
            if let Some(value) = init.get("status").map(Value::resolve)
                && !matches!(value, Value::Undefined)
            {
                let number = value.number()?;
                if !number.is_finite()
                    || number.fract() != 0.0
                    || !(200.0..=599.0).contains(&number)
                {
                    return Err(runtime_error(
                        "Response status must be an integer between 200 and 599",
                    ));
                }
                status = number as u16;
            }
            if let Some(value) = init.get("statusText").map(Value::resolve)
                && !matches!(value, Value::Undefined)
            {
                status_text = value.string_value();
                if !valid_status_text(&status_text) {
                    return Err(runtime_error("Response statusText contains invalid characters"));
                }
            }
            if let Some(value) = init.get("headers").map(Value::resolve)
                && !matches!(value, Value::Undefined | Value::Null)
            {
                headers = header_values_from_init(value)?;
            }
        }
        let body_present = !matches!(body, Value::Undefined | Value::Null);
        let (body, body_mime_type) = body_init_bytes(body)?;
        if body_present && matches!(status, 101 | 103 | 204 | 205 | 304) {
            return Err(runtime_error(
                "Response body is not allowed for this status",
            ));
        }
        let mime_type = inferred_mime_type
            .map(str::to_string)
            .or(body_mime_type);
        if let Some(mime_type) = mime_type
            && !headers.contains_key("content-type")
        {
            headers.insert("content-type".to_string(), mime_type);
        }
        let mime_type = headers.get("content-type").cloned().unwrap_or_default();
        Ok(Value::Response(Rc::new(response_data(
            String::new(),
            status,
            status_text,
            mime_type,
            headers_handle(headers, true),
            body,
            ResponseMetadata {
                redirected: false,
                response_type: "default".to_string(),
            },
        ))))
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

    fn construct_abort_controller(&self) -> Value {
        let signal = Rc::new(RefCell::new(AbortSignalData {
            aborted: false,
            reason: None,
            listeners: Vec::new(),
        }));
        Value::AbortController(Rc::new(RefCell::new(AbortControllerData { signal })))
    }

    fn submit_form(&mut self, path: NodePath) -> Result<(), ScriptError> {
        let form = self
            .document
            .borrow()
            .element_clone(&path)
            .ok_or_else(|| runtime_error("form is no longer in the document"))?;
        let method = form
            .attr("method")
            .map(|method| method.trim().to_ascii_lowercase())
            .unwrap_or_else(|| "get".to_string());
        let enctype = form
            .attr("enctype")
            .map(|enctype| enctype.trim().to_ascii_lowercase())
            .unwrap_or_else(|| "application/x-www-form-urlencoded".to_string());
        if !matches!(method.as_str(), "get" | "post") {
            return Err(runtime_error(
                "only GET and POST form submission are available in the packaged engine",
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
        let url = base
            .resolve(action)
            .map_err(|error| runtime_error(error.to_string()))?;
        let pairs = form_data
            .borrow()
            .pairs
            .iter()
            .filter_map(|(name, value)| match value {
                FormDataEntry::Text(value) => Some((name.clone(), value.clone())),
                FormDataEntry::File(_) => None,
            })
            .collect::<Vec<_>>();
        let encoded = serialize_form_urlencoded(&pairs);
        if method == "get" {
            let url = if encoded.is_empty() {
                url
            } else {
                let query = if url.query().is_empty() {
                    encoded
                } else {
                    format!("{}&{encoded}", url.query())
                };
                url.with_query_fragment(query, url.fragment().to_string())
            };
            self.request_location_navigation(url.to_string(), false);
        } else {
            let (body, content_type) = match enctype.as_str() {
                "application/x-www-form-urlencoded" => (
                    encoded.into_bytes(),
                    "application/x-www-form-urlencoded".to_string(),
                ),
                "multipart/form-data" => {
                    let (body, content_type) = multipart_form_data_body(&form_data);
                    (body, content_type)
                }
                _ => {
                    return Err(runtime_error(format!(
                        "unsupported form enctype {enctype}"
                    )))
                }
            };
            let mut headers = BTreeMap::new();
            headers.insert("content-type".to_string(), content_type);
            let request = ResourceRequest::new(url, "POST", headers, body)
                .map_err(|error| runtime_error(error.to_string()))?;
            self.queue_navigation(NavigationRequest::Submit { request });
        }
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

    fn set_window_scroll(
        &mut self,
        arguments: &[Value],
        relative: bool,
    ) -> Result<(), ScriptError> {
        let current = self.document_scroll_offset();
        let (x, y) = match arguments.first().map(Value::resolve) {
            Some(Value::Object(options)) => {
                let options = options.borrow();
                let x = options
                    .get("left")
                    .or_else(|| options.get("x"))
                    .map(Value::number)
                    .transpose()?
                    .unwrap_or(0.0) as f32;
                let y = options
                    .get("top")
                    .or_else(|| options.get("y"))
                    .map(Value::number)
                    .transpose()?
                    .unwrap_or(0.0) as f32;
                (x, y)
            }
            Some(value) => (
                value.number()? as f32,
                arguments
                    .get(1)
                    .map(Value::number)
                    .transpose()?
                    .unwrap_or(0.0) as f32,
            ),
            None => (0.0, 0.0),
        };
        let offset = if relative {
            (current.0 + x, current.1 + y)
        } else {
            (x, y)
        };
        if self.set_document_scroll_offset(offset) {
            self.dispatch_window_event("scroll")?;
        }
        Ok(())
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
            Builtin::DocumentCreateRange => {
                let path = self
                    .document
                    .borrow()
                    .document_element_path()
                    .unwrap_or_default();
                let range = RangeData {
                    start: RangeBoundary {
                        path: path.clone(),
                        offset: 0,
                    },
                    end: RangeBoundary { path, offset: 0 },
                };
                Ok(Value::Range(Rc::new(RefCell::new(range))))
            }
            Builtin::DocumentGetSelection => Ok(Value::Selection(self.selection.clone())),
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
            Builtin::WebAssemblyInstantiate => {
                let bytes = match arguments.first().map(Value::resolve) {
                    Some(Value::Bytes(bytes)) => bytes.as_ref().clone(),
                    Some(_) => {
                        return Err(runtime_error(
                            "WebAssembly.instantiate expects an ArrayBuffer",
                        ))
                    }
                    None => {
                        return Err(runtime_error(
                            "WebAssembly.instantiate requires module bytes",
                        ))
                    }
                };
                if let Some(imports) = arguments.get(1).map(Value::resolve)
                    && !matches!(imports, Value::Undefined | Value::Object(_))
                {
                    return Err(runtime_error(
                        "WebAssembly.instantiate imports must be an object",
                    ));
                }
                let module = Rc::new(
                    wasm::Module::instantiate(&bytes)
                        .map_err(|error| runtime_error(format!("WebAssembly module error: {error}")))?,
                );
                let mut exports = BTreeMap::new();
                for (name, function_index) in module.exported_functions() {
                    exports.insert(
                        name.to_string(),
                        Value::Builtin(Builtin::WebAssemblyFunction(Rc::new(
                            WasmFunctionData {
                                module: module.clone(),
                                function_index,
                            },
                        ))),
                    );
                }
                let instance = Value::Object(Rc::new(RefCell::new(BTreeMap::from([(
                    "exports".to_string(),
                    Value::Object(Rc::new(RefCell::new(exports))),
                )]))));
                let source = Value::Object(Rc::new(RefCell::new(BTreeMap::from([
                    ("module".to_string(), Value::Object(Rc::new(RefCell::new(BTreeMap::new())))),
                    ("instance".to_string(), instance),
                ]))));
                Ok(Value::Promise(self.fulfilled_promise(source)))
            }
            Builtin::WebAssemblyFunction(function) => {
                let arguments = arguments
                    .iter()
                    .map(|argument| {
                        let number = argument.number()?;
                        if !number.is_finite()
                            || number.fract() != 0.0
                            || number < i32::MIN as f64
                            || number > i32::MAX as f64
                        {
                            return Err(runtime_error(
                                "WebAssembly i32 argument must be a finite integer",
                            ));
                        }
                        Ok(number as i32)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let result = function
                    .module
                    .invoke(function.function_index, &arguments)
                    .map_err(|error| runtime_error(format!("WebAssembly function error: {error}")))?;
                Ok(result
                    .map(|value| Value::Number(value as f64))
                    .unwrap_or(Value::Undefined))
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
            Builtin::WorkerGlobalImportScripts => {
                if !matches!(self.worker_script_type, Some(WorkerScriptType::Classic)) {
                    return Err(runtime_error(
                        "importScripts is only available in classic Workers",
                    ));
                }
                let base_url = self
                    .base_url
                    .clone()
                    .unwrap_or_else(|| self.location.borrow().clone());
                for argument in arguments {
                    let url = base_url
                        .resolve(&argument.string_value())
                        .map_err(|error| runtime_error(error.to_string()))?;
                    let resources = self.resources.clone().unwrap_or_default();
                    let provider = self.resource_provider.clone();
                    let resource = resolve_runtime_resource(&resources, provider.as_deref(), &url)?
                        .ok_or_else(|| {
                            runtime_error(format!("imported worker script {} was not found", url.path()))
                        })?;
                    if !is_javascript(&resource) {
                        return Err(runtime_error(format!(
                            "imported worker script {} has unsupported MIME type {}",
                            url.path(),
                            resource.mime_type()
                        )));
                    }
                    if resource.body().len() > MAX_WORKER_SOURCE_BYTES {
                        return Err(runtime_error(
                            "imported worker script exceeds the 1 MiB limit",
                        ));
                    }
                    let source = String::from_utf8(resource.body().to_vec()).map_err(|_| {
                        runtime_error(format!(
                            "imported worker script {} is not UTF-8",
                            url.path()
                        ))
                    })?;
                    self.execute(&source)?;
                }
                Ok(Value::Undefined)
            }
            Builtin::WorkerAddEventListener(worker) => {
                let event = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let callback = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                if !matches!(callback.resolve(), Value::Function(_)) {
                    return Err(runtime_error("Worker event listener must be a function"));
                }
                worker.borrow_mut().listeners.push(EventListener { event, callback });
                Ok(Value::Undefined)
            }
            Builtin::WorkerPostMessage(worker) => {
                if arguments.len() > 1
                    && !matches!(arguments.get(1).map(Value::resolve), Some(Value::Undefined))
                {
                    return Err(runtime_error(
                        "Worker transfer lists are not supported in this slice",
                    ));
                }
                let message = serialize_json(
                    arguments
                        .first()
                        .unwrap_or(&Value::Undefined),
                )?;
                let worker = worker.borrow();
                if worker.terminated {
                    return Err(runtime_error("Worker has been terminated"));
                }
                worker
                    .command_sender
                    .try_send(WorkerCommand::Message(message))
                    .map_err(|error| match error {
                        TrySendError::Full(_) => runtime_error("Worker message queue is full"),
                        TrySendError::Disconnected(_) => runtime_error("Worker is unavailable"),
                    })?;
                Ok(Value::Undefined)
            }
            Builtin::WorkerTerminate(worker) => {
                let mut worker = worker.borrow_mut();
                if !worker.terminated {
                    worker.termination_flag.store(true, Ordering::Release);
                    let _ = worker.command_sender.try_send(WorkerCommand::Terminate);
                    worker.terminated = true;
                }
                Ok(Value::Undefined)
            }
            Builtin::WorkerGlobalPostMessage(sender) => {
                if arguments.len() > 1
                    && !matches!(arguments.get(1).map(Value::resolve), Some(Value::Undefined))
                {
                    return Err(runtime_error(
                        "Worker transfer lists are not supported in this slice",
                    ));
                }
                let message = serialize_json(
                    arguments
                        .first()
                        .unwrap_or(&Value::Undefined),
                )?;
                sender
                    .try_send(WorkerEvent::Message(message))
                    .map_err(|error| match error {
                        TrySendError::Full(_) => runtime_error("Worker message queue is full"),
                        TrySendError::Disconnected(_) => runtime_error("Worker is unavailable"),
                    })?;
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
            Builtin::DocumentAddEventListener => {
                let event = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let callback = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                if !matches!(callback, Value::Function(_)) {
                    return Err(runtime_error("document event listener must be a function"));
                }
                self.listeners
                    .entry(Vec::new())
                    .or_default()
                    .push(EventListener { event, callback });
                Ok(Value::Undefined)
            }
            Builtin::WindowScrollTo => {
                self.set_window_scroll(&arguments, false)?;
                Ok(Value::Undefined)
            }
            Builtin::WindowScrollBy => {
                self.set_window_scroll(&arguments, true)?;
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
            Builtin::WindowGetSelection => Ok(Value::Selection(self.selection.clone())),
            Builtin::ElementSelect(path) => {
                self.set_text_selection(path, 0, usize::MAX, SelectionDirection::None)?;
                Ok(Value::Undefined)
            }
            Builtin::ElementSetSelectionRange(path) => {
                let start = arguments
                    .first()
                    .map(Value::number)
                    .transpose()?
                    .map(selection_offset)
                    .unwrap_or_default();
                let end = arguments
                    .get(1)
                    .map(Value::number)
                    .transpose()?
                    .map(selection_offset)
                    .unwrap_or(start);
                let direction = arguments
                    .get(2)
                    .map(Value::string_value)
                    .unwrap_or_default();
                self.set_text_selection(
                    path,
                    start,
                    end,
                    selection_direction(&direction),
                )?;
                Ok(Value::Undefined)
            }
            Builtin::ElementSetRangeText(path) => {
                self.set_range_text(&path, &arguments)?;
                Ok(Value::Undefined)
            }
            Builtin::RangeSetStart(range) => {
                let boundary = self.range_boundary_from_arguments(&arguments)?;
                let changed = {
                    let mut range_data = range.borrow_mut();
                    let previous = range_data.clone();
                    range_data.start = boundary.clone();
                    if range_data.start.path == range_data.end.path
                        && range_data.start.offset > range_data.end.offset
                    {
                        range_data.end = boundary;
                    }
                    *range_data != previous
                };
                if changed && self.selection_uses_range(&range) {
                    self.dispatch_document_selection_change()?;
                }
                Ok(Value::Undefined)
            }
            Builtin::RangeSetEnd(range) => {
                let boundary = self.range_boundary_from_arguments(&arguments)?;
                let changed = {
                    let mut range_data = range.borrow_mut();
                    let previous = range_data.clone();
                    range_data.end = boundary.clone();
                    if range_data.start.path == range_data.end.path
                        && range_data.end.offset < range_data.start.offset
                    {
                        range_data.start = boundary;
                    }
                    *range_data != previous
                };
                if changed && self.selection_uses_range(&range) {
                    self.dispatch_document_selection_change()?;
                }
                Ok(Value::Undefined)
            }
            Builtin::RangeCollapse(range) => {
                let to_start = arguments.first().map(Value::truthy).unwrap_or(false);
                let changed = {
                    let mut range_data = range.borrow_mut();
                    let previous = range_data.clone();
                    if to_start {
                        range_data.end = range_data.start.clone();
                    } else {
                        range_data.start = range_data.end.clone();
                    }
                    *range_data != previous
                };
                if changed && self.selection_uses_range(&range) {
                    self.dispatch_document_selection_change()?;
                }
                Ok(Value::Undefined)
            }
            Builtin::RangeSelectNodeContents(range) => {
                let boundary = self.range_boundary_from_arguments(&arguments)?;
                let length = self
                    .document
                    .borrow()
                    .element_text_content(&boundary.path)
                    .map(|text| utf16_len(&text))
                    .unwrap_or(0);
                let changed = {
                    let mut range_data = range.borrow_mut();
                    let previous = range_data.clone();
                    range_data.start = RangeBoundary {
                        path: boundary.path.clone(),
                        offset: 0,
                    };
                    range_data.end = RangeBoundary {
                        path: boundary.path,
                        offset: length,
                    };
                    *range_data != previous
                };
                if changed && self.selection_uses_range(&range) {
                    self.dispatch_document_selection_change()?;
                }
                Ok(Value::Undefined)
            }
            Builtin::RangeClone(range) => Ok(Value::Range(Rc::new(RefCell::new(
                range.borrow().clone(),
            )))),
            Builtin::RangeToString(range) => Ok(Value::String(self.range_to_string(&range))),
            Builtin::RangeDetach => Ok(Value::Undefined),
            Builtin::SelectionAddRange(selection) => {
                let Some(Value::Range(range)) = arguments.first().map(Value::resolve) else {
                    return Err(runtime_error("Selection.addRange expects a Range"));
                };
                let changed = selection
                    .borrow()
                    .range
                    .as_ref()
                    .is_none_or(|current| !Rc::ptr_eq(current, &range));
                selection.borrow_mut().range = Some(range);
                if changed && Rc::ptr_eq(&selection, &self.selection) {
                    self.dispatch_document_selection_change()?;
                }
                Ok(Value::Undefined)
            }
            Builtin::SelectionRemoveAllRanges(selection) => {
                let changed = selection.borrow().range.is_some();
                selection.borrow_mut().range = None;
                if changed && Rc::ptr_eq(&selection, &self.selection) {
                    self.dispatch_document_selection_change()?;
                }
                Ok(Value::Undefined)
            }
            Builtin::SelectionGetRangeAt(selection) => {
                let index = arguments
                    .first()
                    .map(Value::number)
                    .transpose()?
                    .unwrap_or(0.0);
                if !index.is_finite() || index.fract() != 0.0 || index < 0.0 {
                    return Err(runtime_error("Selection range index must be a non-negative integer"));
                }
                if index != 0.0 {
                    return Err(runtime_error("Selection range index is out of bounds"));
                }
                selection
                    .borrow()
                    .range
                    .clone()
                    .map(Value::Range)
                    .ok_or_else(|| runtime_error("Selection has no range at index 0"))
            }
            Builtin::SelectionCollapse(selection) => {
                let Some(node) = arguments.first().map(Value::resolve) else {
                    let changed = selection.borrow().range.is_some();
                    selection.borrow_mut().range = None;
                    if changed && Rc::ptr_eq(&selection, &self.selection) {
                        self.dispatch_document_selection_change()?;
                    }
                    return Ok(Value::Undefined);
                };
                if matches!(node, Value::Null | Value::Undefined) {
                    let changed = selection.borrow().range.is_some();
                    selection.borrow_mut().range = None;
                    if changed && Rc::ptr_eq(&selection, &self.selection) {
                        self.dispatch_document_selection_change()?;
                    }
                    return Ok(Value::Undefined);
                }
                let Value::Element(handle) = node else {
                    return Err(runtime_error("Selection.collapse expects an Element or null"));
                };
                let Some(path) = attached_path(&handle) else {
                    return Err(runtime_error("Selection.collapse requires an attached Element"));
                };
                let value = self
                    .document
                    .borrow()
                    .element_text_content(&path)
                    .unwrap_or_default();
                let offset = arguments
                    .get(1)
                    .map(Value::number)
                    .transpose()?
                    .map(selection_offset)
                    .unwrap_or_default()
                    .min(utf16_len(&value));
                let range = RangeData {
                    start: RangeBoundary {
                        path: path.clone(),
                        offset,
                    },
                    end: RangeBoundary { path, offset },
                };
                let changed = selection
                    .borrow()
                    .range
                    .as_ref()
                    .is_none_or(|current| *current.borrow() != range);
                selection.borrow_mut().range = Some(Rc::new(RefCell::new(range)));
                if changed && Rc::ptr_eq(&selection, &self.selection) {
                    self.dispatch_document_selection_change()?;
                }
                Ok(Value::Undefined)
            }
            Builtin::SelectionToString(selection) => {
                let range = selection.borrow().range.clone();
                Ok(Value::String(
                    range
                        .as_ref()
                        .map(|range| self.range_to_string(range))
                        .unwrap_or_default(),
                ))
            }
            Builtin::ElementRequestSubmit(path) => {
                self.request_submit(path)?;
                Ok(Value::Undefined)
            }
            Builtin::ElementCheckValidity(path) | Builtin::ElementReportValidity(path) => {
                Ok(Value::Bool(self.check_validity_path(&path)?))
            }
            Builtin::ElementSetCustomValidity(path) => {
                let message = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                self.set_custom_validity(path, message);
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
            Builtin::UrlSearchParamsKeys(params) => Ok(iterator_value(
                params
                    .borrow()
                    .pairs
                    .iter()
                    .map(|(name, _)| Value::String(name.clone()))
                    .collect(),
            )),
            Builtin::UrlSearchParamsValues(params) => Ok(iterator_value(
                params
                    .borrow()
                    .pairs
                    .iter()
                    .map(|(_, value)| Value::String(value.clone()))
                    .collect(),
            )),
            Builtin::UrlSearchParamsEntries(params) => Ok(iterator_value(
                params
                    .borrow()
                    .pairs
                    .iter()
                    .map(|(name, value)| {
                        Value::Array(Rc::new(RefCell::new(vec![
                            Value::String(name.clone()),
                            Value::String(value.clone()),
                        ])))
                    })
                    .collect(),
            )),
            Builtin::UrlSearchParamsForEach(params) => {
                let callback = arguments.first().cloned().unwrap_or(Value::Undefined);
                if !matches!(callback, Value::Function(_)) {
                    return Err(runtime_error(
                        "URLSearchParams.forEach callback must be a function",
                    ));
                }
                let pairs = params.borrow().pairs.clone();
                for (name, value) in pairs {
                    self.call(
                        callback.clone(),
                        vec![
                            Value::String(value),
                            Value::String(name),
                            Value::UrlSearchParams(params.clone()),
                        ],
                    )?;
                }
                Ok(Value::Undefined)
            }
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
                let value = form_data_entry(
                    arguments.get(1),
                    form_data_filename(arguments.get(2)),
                );
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
                    .map(|(_, value)| form_data_entry_value(value))
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
                        .map(|(_, value)| form_data_entry_value(value))
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
                let value = form_data_entry(
                    arguments.get(1),
                    form_data_filename(arguments.get(2)),
                );
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
            Builtin::FormDataKeys(form_data) => Ok(iterator_value(
                form_data
                    .borrow()
                    .pairs
                    .iter()
                    .map(|(name, _)| Value::String(name.clone()))
                    .collect(),
            )),
            Builtin::FormDataValues(form_data) => Ok(iterator_value(
                form_data
                    .borrow()
                    .pairs
                    .iter()
                    .map(|(_, value)| form_data_entry_value(value))
                    .collect(),
            )),
            Builtin::FormDataEntries(form_data) => Ok(iterator_value(
                form_data
                    .borrow()
                    .pairs
                    .iter()
                    .map(|(name, value)| {
                        Value::Array(Rc::new(RefCell::new(vec![
                            Value::String(name.clone()),
                            form_data_entry_value(value),
                        ])))
                    })
                    .collect(),
            )),
            Builtin::FormDataForEach(form_data) => {
                let callback = arguments.first().cloned().unwrap_or(Value::Undefined);
                if !matches!(callback, Value::Function(_)) {
                    return Err(runtime_error("FormData.forEach callback must be a function"));
                }
                let entries = form_data.borrow().pairs.clone();
                for (name, value) in entries {
                    self.call(
                        callback.clone(),
                        vec![
                            form_data_entry_value(&value),
                            Value::String(name),
                            Value::FormData(form_data.clone()),
                        ],
                    )?;
                }
                Ok(Value::Undefined)
            }
            Builtin::BlobSlice(blob) => {
                let (start, end) = blob_slice_range(&arguments, blob.bytes.len() as i64)?;
                let mime_type = arguments
                    .get(2)
                    .map(Value::resolve)
                    .and_then(|value| match value {
                        Value::Undefined => None,
                        value => Some(normalize_blob_type(&value.string_value())),
                    })
                    .unwrap_or_default();
                Ok(Value::Blob(Rc::new(BlobData {
                    bytes: blob.bytes[start..end].to_vec(),
                    mime_type,
                })))
            }
            Builtin::BlobText(blob) => Ok(Value::Promise(self.fulfilled_promise(
                Value::String(String::from_utf8_lossy(&blob.bytes).into_owned()),
            ))),
            Builtin::BlobBytes(blob) => Ok(Value::Promise(self.fulfilled_promise(Value::Bytes(
                Rc::new(blob.bytes.clone()),
            )))),
            Builtin::FileSlice(file) => {
                let (start, end) = blob_slice_range(&arguments, file.bytes.len() as i64)?;
                let mime_type = arguments
                    .get(2)
                    .map(Value::resolve)
                    .and_then(|value| match value {
                        Value::Undefined => None,
                        value => Some(normalize_blob_type(&value.string_value())),
                    })
                    .unwrap_or_default();
                Ok(Value::Blob(Rc::new(BlobData {
                    bytes: file.bytes[start..end].to_vec(),
                    mime_type,
                })))
            }
            Builtin::FileText(file) => Ok(Value::Promise(self.fulfilled_promise(
                Value::String(String::from_utf8_lossy(&file.bytes).into_owned()),
            ))),
            Builtin::FileBytes(file) => Ok(Value::Promise(self.fulfilled_promise(Value::Bytes(
                Rc::new(file.bytes.clone()),
            )))),
            Builtin::FileListItem(files) => {
                let index = arguments
                    .first()
                    .map(Value::number)
                    .transpose()?
                    .unwrap_or(-1.0);
                if !index.is_finite() || index < 0.0 || index.fract() != 0.0 {
                    return Ok(Value::Null);
                }
                Ok(files
                    .get(index as usize)
                    .cloned()
                    .map(Value::File)
                    .unwrap_or(Value::Null))
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
            Builtin::ClipboardReadText => {
                let Some(provider) = self.clipboard_provider.as_ref() else {
                    return Ok(Value::Promise(self.rejected_promise(
                        "NotAllowedError: clipboard access is unavailable",
                    )));
                };
                match provider.read_text() {
                    Ok(text) => Ok(Value::Promise(
                        self.fulfilled_promise(Value::String(text)),
                    )),
                    Err(error) => Ok(Value::Promise(self.rejected_promise(error.to_string()))),
                }
            }
            Builtin::ClipboardWriteText => {
                let text = arguments
                    .first()
                    .ok_or_else(|| runtime_error("Clipboard.writeText requires text"))?
                    .string_value();
                let Some(provider) = self.clipboard_provider.as_ref() else {
                    return Ok(Value::Promise(self.rejected_promise(
                        "NotAllowedError: clipboard access is unavailable",
                    )));
                };
                match provider.write_text(&text) {
                    Ok(()) => Ok(Value::Promise(self.fulfilled_promise(Value::Undefined))),
                    Err(error) => Ok(Value::Promise(self.rejected_promise(error.to_string()))),
                }
            }
            Builtin::Fetch => Ok(Value::Promise(self.fetch(&arguments))),
            Builtin::PromiseThen(promise) => {
                self.promise_then(promise, arguments.first(), arguments.get(1))
            }
            Builtin::PromiseCatch(promise) => self.promise_then(promise, None, arguments.first()),
            Builtin::RequestClone(request) => {
                Ok(Value::Request(clone_request(&request)?))
            }
            Builtin::RequestText(request) => match self.consume_request_body(&request) {
                Ok(body) => Ok(Value::Promise(self.fulfilled_promise(Value::String(
                    String::from_utf8_lossy(&body).into_owned(),
                )))),
                Err(error) => Ok(Value::Promise(self.rejected_promise(error.message))),
            },
            Builtin::RequestBytes(request) => match self.consume_request_body(&request) {
                Ok(body) => Ok(Value::Promise(self.fulfilled_promise(Value::Bytes(Rc::new(
                    body,
                ))))),
                Err(error) => Ok(Value::Promise(self.rejected_promise(error.message))),
            },
            Builtin::RequestBlob(request) => match self.consume_request_body(&request) {
                Ok(body) => Ok(Value::Promise(self.fulfilled_promise(Value::Blob(Rc::new(
                    BlobData {
                        bytes: body,
                        mime_type: request
                            .headers
                            .borrow()
                            .values
                            .get("content-type")
                            .cloned()
                            .unwrap_or_default(),
                    },
                ))))),
                Err(error) => Ok(Value::Promise(self.rejected_promise(error.message))),
            },
            Builtin::RequestFormData(request) => {
                let body = match self.consume_request_body(&request) {
                    Ok(body) => body,
                    Err(error) => return Ok(Value::Promise(self.rejected_promise(error.message))),
                };
                let mime_type = request
                    .headers
                    .borrow()
                    .values
                    .get("content-type")
                    .cloned()
                    .unwrap_or_default();
                match form_data_from_body(&body, &mime_type) {
                    Ok(form_data) => Ok(Value::Promise(self.fulfilled_promise(form_data))),
                    Err(error) => Ok(Value::Promise(self.rejected_promise(error.message))),
                }
            }
            Builtin::HeadersAppend(headers) => {
                let (name, value) = normalized_header_pair(&arguments)?;
                let mut headers = headers.borrow_mut();
                if !headers.mutable {
                    return Err(runtime_error("headers are immutable"));
                }
                if let Some(existing) = headers.values.get_mut(&name) {
                    existing.push_str(", ");
                    existing.push_str(&value);
                } else {
                    headers.values.insert(name, value);
                }
                Ok(Value::Undefined)
            }
            Builtin::HeadersDelete(headers) => {
                let name = normalized_header_name(&arguments)?;
                let mut headers = headers.borrow_mut();
                if !headers.mutable {
                    return Err(runtime_error("headers are immutable"));
                }
                headers.values.remove(&name);
                Ok(Value::Undefined)
            }
            Builtin::HeadersGet(headers) => {
                let name = normalized_header_name(&arguments)?;
                Ok(headers
                    .borrow()
                    .values
                    .get(&name)
                    .cloned()
                    .map(Value::String)
                    .unwrap_or(Value::Null))
            }
            Builtin::HeadersHas(headers) => {
                let name = normalized_header_name(&arguments)?;
                Ok(Value::Bool(headers.borrow().values.contains_key(&name)))
            }
            Builtin::HeadersKeys(headers) => Ok(iterator_value(
                headers
                    .borrow()
                    .values
                    .keys()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            )),
            Builtin::HeadersValues(headers) => Ok(iterator_value(
                headers
                    .borrow()
                    .values
                    .values()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            )),
            Builtin::HeadersEntries(headers) => Ok(iterator_value(
                headers
                    .borrow()
                    .values
                    .iter()
                    .map(|(name, value)| {
                        Value::Array(Rc::new(RefCell::new(vec![
                            Value::String(name.clone()),
                            Value::String(value.clone()),
                        ])))
                    })
                    .collect(),
            )),
            Builtin::HeadersForEach(headers) => {
                let callback = arguments.first().cloned().unwrap_or(Value::Undefined);
                if !matches!(callback, Value::Function(_)) {
                    return Err(runtime_error("headers.forEach callback must be a function"));
                }
                let entries = headers
                    .borrow()
                    .values
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect::<Vec<_>>();
                for (name, value) in entries {
                    self.call(
                        callback.clone(),
                        vec![
                            Value::String(value),
                            Value::String(name),
                            Value::Headers(headers.clone()),
                        ],
                    )?;
                }
                Ok(Value::Undefined)
            }
            Builtin::HeadersSet(headers) => {
                let (name, value) = normalized_header_pair(&arguments)?;
                let mut headers = headers.borrow_mut();
                if !headers.mutable {
                    return Err(runtime_error("headers are immutable"));
                }
                headers.values.insert(name, value);
                Ok(Value::Undefined)
            }
            Builtin::ResponseErrorConstructor => Ok(Value::Response(Rc::new(response_data(
                String::new(),
                0,
                String::new(),
                String::new(),
                headers_handle(BTreeMap::new(), false),
                Vec::new(),
                ResponseMetadata {
                    redirected: false,
                    response_type: "error".to_string(),
                },
            )))),
            Builtin::ResponseJsonConstructor => {
                let data = arguments.first().cloned().unwrap_or(Value::Undefined);
                let body = serialize_json(&data)?;
                let init = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                self.construct_response_parts(
                    Value::String(body),
                    init,
                    Some("application/json"),
                )
            }
            Builtin::ResponseRedirectConstructor => {
                let reference = arguments
                    .first()
                    .map(Value::string_value)
                    .ok_or_else(|| runtime_error("Response.redirect requires a URL"))?;
                let base = self.base_url.clone().unwrap_or_else(|| {
                    AppUrl::parse("https://app.ceri.invalid/")
                        .expect("built-in app origin should parse")
                });
                let url = base
                    .resolve(&reference)
                    .map_err(|error| runtime_error(error.to_string()))?;
                let status = arguments
                    .get(1)
                    .map(Value::number)
                    .transpose()?
                    .unwrap_or(302.0);
                if !status.is_finite()
                    || status.fract() != 0.0
                    || !(0.0..=u16::MAX as f64).contains(&status)
                {
                    return Err(runtime_error("Response redirect status is invalid"));
                }
                let status = status as u16;
                if !matches!(status, 301 | 302 | 303 | 307 | 308) {
                    return Err(runtime_error("Response redirect status is invalid"));
                }
                let mut headers = BTreeMap::new();
                headers.insert("location".to_string(), url.to_string());
                Ok(Value::Response(Rc::new(response_data(
                    String::new(),
                    status,
                    String::new(),
                    String::new(),
                    headers_handle(headers, false),
                    Vec::new(),
                    ResponseMetadata {
                        redirected: false,
                        response_type: "default".to_string(),
                    },
                ))))
            }
            Builtin::ResponseClone(response) => {
                let body = response.body.borrow();
                if body.consumed || body.disturbed || body.locked {
                    return Err(runtime_error("response body is already used"));
                }
                let headers = {
                    let headers = response.headers.borrow();
                    headers_handle(headers.values.clone(), headers.mutable)
                };
                let cloned = response_data(
                    response.url.clone(),
                    response.status,
                    response.status_text.clone(),
                    response.mime_type.clone(),
                    headers,
                    body.bytes[body.cursor..].to_vec(),
                    ResponseMetadata {
                        redirected: response.redirected,
                        response_type: response.response_type.clone(),
                    },
                );
                Ok(Value::Response(Rc::new(cloned)))
            }
            Builtin::ResponseText(response) => match self.consume_response_body(&response) {
                Ok(body) => Ok(Value::Promise(self.fulfilled_promise(Value::String(
                    String::from_utf8_lossy(&body).into_owned(),
                )))),
                Err(error) => Ok(Value::Promise(self.rejected_promise(error.message))),
            },
            Builtin::ResponseJson(response) => {
                let body = match self.consume_response_body(&response) {
                    Ok(body) => body,
                    Err(error) => return Ok(Value::Promise(self.rejected_promise(error.message))),
                };
                let source = String::from_utf8_lossy(&body);
                match self.parse_json(&source) {
                    Ok(value) => Ok(Value::Promise(self.fulfilled_promise(value))),
                    Err(error) => Ok(Value::Promise(self.rejected_promise(error.message))),
                }
            }
            Builtin::ResponseBytes(response) => match self.consume_response_body(&response) {
                Ok(body) => Ok(Value::Promise(self.fulfilled_promise(Value::Bytes(Rc::new(
                    body,
                ))))),
                Err(error) => Ok(Value::Promise(self.rejected_promise(error.message))),
            },
            Builtin::ResponseBlob(response) => match self.consume_response_body(&response) {
                Ok(body) => Ok(Value::Promise(self.fulfilled_promise(Value::Blob(Rc::new(
                    BlobData {
                        bytes: body,
                        mime_type: response.mime_type.clone(),
                    },
                ))))),
                Err(error) => Ok(Value::Promise(self.rejected_promise(error.message))),
            },
            Builtin::ResponseFormData(response) => {
                let body = match self.consume_response_body(&response) {
                    Ok(body) => body,
                    Err(error) => return Ok(Value::Promise(self.rejected_promise(error.message))),
                };
                match form_data_from_body(&body, &response.mime_type) {
                    Ok(form_data) => Ok(Value::Promise(self.fulfilled_promise(form_data))),
                    Err(error) => Ok(Value::Promise(self.rejected_promise(error.message))),
                }
            }
            Builtin::ReadableStreamGetReader(stream) => {
                let body = stream.borrow().body.clone();
                let mut body_state = body.borrow_mut();
                if body_state.locked || body_state.disturbed || body_state.consumed {
                    return Err(runtime_error("readable stream is already used"));
                }
                body_state.locked = true;
                drop(body_state);
                Ok(Value::ReadableStreamReader(Rc::new(RefCell::new(
                    ReadableStreamReaderData {
                        body,
                        released: false,
                    },
                ))))
            }
            Builtin::ReadableStreamCancel(stream) => {
                let mut stream = stream.borrow_mut();
                if stream.canceled {
                    return Ok(Value::Promise(self.fulfilled_promise(Value::Undefined)));
                }
                {
                    let mut body = stream.body.borrow_mut();
                    body.cursor = body.bytes.len();
                    body.consumed = true;
                    body.disturbed = true;
                    body.locked = false;
                }
                stream.canceled = true;
                Ok(Value::Promise(self.fulfilled_promise(Value::Undefined)))
            }
            Builtin::ReadableStreamTee(stream) => {
                let body = stream.borrow().body.clone();
                let mut body_state = body.borrow_mut();
                if body_state.locked || body_state.disturbed || body_state.consumed {
                    return Err(runtime_error("readable stream is already used"));
                }
                let remaining = body_state.bytes[body_state.cursor..].to_vec();
                body_state.cursor = body_state.bytes.len();
                body_state.disturbed = true;
                body_state.locked = true;
                drop(body_state);
                let left = Value::ReadableStream(Rc::new(RefCell::new(ReadableStreamData {
                    body: response_body(remaining.clone()),
                    canceled: false,
                })));
                let right = Value::ReadableStream(Rc::new(RefCell::new(ReadableStreamData {
                    body: response_body(remaining),
                    canceled: false,
                })));
                Ok(Value::Array(Rc::new(RefCell::new(vec![left, right]))))
            }
            Builtin::ReadableStreamReaderRead(reader) => {
                let reader = reader.borrow();
                if reader.released {
                    return Ok(Value::Promise(
                        self.rejected_promise("readable stream reader has been released"),
                    ));
                }
                let mut body = reader.body.borrow_mut();
                if body.cursor >= body.bytes.len() {
                    body.consumed = true;
                    body.disturbed = true;
                    return Ok(Value::Promise(self.fulfilled_promise(read_result(
                        Value::Undefined,
                        true,
                    ))));
                }
                body.disturbed = true;
                let end = body.cursor.saturating_add(16).min(body.bytes.len());
                let chunk = Rc::new(body.bytes[body.cursor..end].to_vec());
                body.cursor = end;
                Ok(Value::Promise(self.fulfilled_promise(read_result(
                    Value::Bytes(chunk),
                    false,
                ))))
            }
            Builtin::ReadableStreamReaderReleaseLock(reader) => {
                let mut reader = reader.borrow_mut();
                if !reader.released {
                    reader.body.borrow_mut().locked = false;
                    reader.released = true;
                }
                Ok(Value::Undefined)
            }
            Builtin::ReadableStreamReaderCancel(reader) => {
                let mut reader = reader.borrow_mut();
                if reader.released {
                    return Ok(Value::Promise(
                        self.rejected_promise("readable stream reader has been released"),
                    ));
                }
                {
                    let mut body = reader.body.borrow_mut();
                    body.cursor = body.bytes.len();
                    body.consumed = true;
                    body.disturbed = true;
                    body.locked = false;
                }
                reader.released = true;
                Ok(Value::Promise(self.fulfilled_promise(Value::Undefined)))
            }
            Builtin::AbortControllerAbort(controller) => {
                let reason = arguments
                    .first()
                    .cloned()
                    .unwrap_or_else(abort_error_value);
                let signal = controller.borrow().signal.clone();
                let listeners = {
                    let mut signal = signal.borrow_mut();
                    if signal.aborted {
                        return Ok(Value::Undefined);
                    }
                    signal.aborted = true;
                    signal.reason = Some(reason);
                    std::mem::take(&mut signal.listeners)
                };
                for listener in listeners {
                    self.call(listener, vec![abort_event_value()])?;
                }
                Ok(Value::Undefined)
            }
            Builtin::AbortSignalAddEventListener(signal) => {
                let event = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                if event != "abort" {
                    return Err(runtime_error(
                        "AbortSignal only supports abort event listeners",
                    ));
                }
                let callback = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                if !matches!(callback, Value::Function(_)) {
                    return Err(runtime_error("AbortSignal listener must be a function"));
                }
                if !signal.borrow().aborted {
                    signal.borrow_mut().listeners.push(callback);
                }
                Ok(Value::Undefined)
            }
            Builtin::AbortSignalThrowIfAborted(signal) => {
                if let Some(reason) = signal.borrow().reason.clone() {
                    self.pending_exception = Some(error_handle(&reason));
                    return Err(runtime_error(reason.string_value()));
                }
                Ok(Value::Undefined)
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

    fn dispatch_non_bubbling_event(
        &mut self,
        path: NodePath,
        event: &str,
    ) -> Result<EventDispatch, ScriptError> {
        let callbacks = self
            .listeners
            .get(&path)
            .into_iter()
            .flatten()
            .filter(|listener| listener.event == event)
            .map(|listener| listener.callback.clone())
            .collect::<Vec<_>>();
        let state = Rc::new(RefCell::new(EventState::default()));
        let event_value = Value::Event(EventData {
            event_type: event.to_string(),
            target: path.clone(),
            current_target: path,
            state: state.clone(),
            history_state: Rc::new(RefCell::new(Value::Null)),
            key: String::new(),
            data: String::new(),
            modifiers: KeyModifiers::default(),
            is_composing: self.composition.is_some(),
            data_value: None,
            error_message: None,
        });
        for callback in callbacks.iter().cloned() {
            self.call(callback, vec![event_value.clone()])?;
        }
        Ok(EventDispatch {
            handled: !callbacks.is_empty(),
            default_prevented: state.borrow().default_prevented,
        })
    }

    fn selection_uses_range(&self, range: &RangeHandle) -> bool {
        self.selection
            .borrow()
            .range
            .as_ref()
            .is_some_and(|active| Rc::ptr_eq(active, range))
    }

    fn dispatch_document_selection_change(&mut self) -> Result<(), ScriptError> {
        self.dispatch_event(Vec::new(), "selectionchange")?;
        self.drain_microtasks()?;
        Ok(())
    }

    fn dispatch_event_with_details(
        &mut self,
        path: NodePath,
        event: &str,
        key: &str,
        data: &str,
    ) -> Result<EventDispatch, ScriptError> {
        self.dispatch_event_with_modifiers(path, event, key, data, KeyModifiers::default())
    }

    fn dispatch_event_with_modifiers(
        &mut self,
        path: NodePath,
        event: &str,
        key: &str,
        data: &str,
        modifiers: KeyModifiers,
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
                modifiers,
                is_composing: self.composition.is_some(),
                data_value: None,
                error_message: None,
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

    fn fetch(&self, arguments: &[Value]) -> PromiseHandle {
        let input = arguments
            .first()
            .cloned()
            .unwrap_or(Value::Undefined)
            .resolve();
        let (url, request_input) = match input {
            Value::Request(request) => (request.url.clone(), Some(request)),
            input => {
                let reference = input.string_value();
                let Some(base_url) = &self.base_url else {
                    return self.rejected_promise("fetch is unavailable without a document URL");
                };
                let Ok(url) = base_url.resolve(&reference) else {
                    return self.rejected_promise(format!("fetch rejected URL {reference}"));
                };
                (url, None)
            }
        };
        let options = match parse_fetch_options(arguments.get(1), request_input.as_ref()) {
            Ok(options) => options,
            Err(error) => return self.rejected_promise(error.message),
        };
        if let Some(signal) = options.signal.as_ref() {
            let signal = signal.borrow();
            if signal.aborted {
                return self.rejected_promise_value(
                    signal.reason.clone().unwrap_or_else(abort_error_value),
                );
            }
        }
        if options.body_from_request {
            let Some(request_input) = request_input.as_ref() else {
                return self.rejected_promise("fetch request body source is unavailable");
            };
            if let Err(error) = self.consume_request_body(request_input) {
                return self.rejected_promise(error.message);
            }
        }
        let request = match ResourceRequest::new(
            url.clone(),
            options.method,
            options.headers,
            options.body,
        ) {
            Ok(request) => request.with_cache_mode(options.cache_mode),
            Err(error) => return self.rejected_promise(error.to_string()),
        };
        if self.resources.is_none() && self.resource_provider.is_none() {
            return self.rejected_promise("fetch is unavailable without a resource bundle");
        }
        let resources = self.resources.clone().unwrap_or_default();
        let response = match resolve_runtime_response_with_redirects(
            &resources,
            self.resource_provider.as_deref(),
            &request,
            options.redirect,
        ) {
            Ok(Some(response)) => response,
            Ok(None) => {
                return self.fulfilled_promise(Value::Response(Rc::new(response_data(
                    url.to_string(),
                    404,
                    String::new(),
                    String::new(),
                    headers_handle(BTreeMap::new(), false),
                    Vec::new(),
                    ResponseMetadata {
                        redirected: false,
                        response_type: "basic".to_string(),
                    },
                ))));
            }
            Err(error) => return self.rejected_promise(error.message),
        };
        self.fulfilled_promise(Value::Response(Rc::new(response_data(
            response.url.to_string(),
            response.response.status(),
            String::new(),
            response.response.mime_type().to_string(),
            headers_handle(response.response.headers().clone(), false),
            response.response.body().to_vec(),
            ResponseMetadata {
                redirected: response.redirected,
                response_type: "basic".to_string(),
            },
        ))))
    }

    fn consume_response_body(&self, response: &ResponseHandle) -> Result<Vec<u8>, ScriptError> {
        let mut body = response.body.borrow_mut();
        if body.consumed || body.disturbed || body.locked {
            return Err(runtime_error("response body is already used"));
        }
        body.cursor = body.bytes.len();
        body.consumed = true;
        body.disturbed = true;
        Ok(body.bytes.clone())
    }

    fn consume_request_body(&self, request: &RequestHandle) -> Result<Vec<u8>, ScriptError> {
        let mut body = request.body.borrow_mut();
        if body.consumed || body.disturbed || body.locked {
            return Err(runtime_error("request body is already used"));
        }
        body.cursor = body.bytes.len();
        body.consumed = true;
        body.disturbed = true;
        Ok(body.bytes.clone())
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

fn collect_sequential_focus_nodes(
    node: &AccessibilityNode,
    order: &mut usize,
    entries: &mut Vec<(i32, usize, NodePath)>,
    document: &Rc<RefCell<Document>>,
) {
    if node.focusable {
        let tabindex = document
            .borrow()
            .element_clone(&node.path)
            .and_then(|element| element.attr("tabindex").and_then(|value| value.parse().ok()))
            .unwrap_or(0);
        entries.push((tabindex, *order, node.path.clone()));
        *order += 1;
    }
    for child in &node.children {
        collect_sequential_focus_nodes(child, order, entries, document);
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
            Self::Validity(_) => "[object ValidityState]".to_string(),
            Self::Range(_) => "[object Range]".to_string(),
            Self::Selection(_) => "[object Selection]".to_string(),
            Self::Dataset(_) => "[object DOMStringMap]".to_string(),
            Self::Error(error) => error.message.clone(),
            Self::Style(_) => "[style]".to_string(),
            Self::ClassList(_) => "[classList]".to_string(),
            Self::Event(_) => "[event]".to_string(),
            Self::Request(_) => "[object Request]".to_string(),
            Self::Response(_) => "[response]".to_string(),
            Self::Bytes(_) => "[object ArrayBuffer]".to_string(),
            Self::ReadableStream(_) => "[object ReadableStream]".to_string(),
            Self::ReadableStreamReader(_) => "[object ReadableStreamDefaultReader]".to_string(),
            Self::Headers(_) => "[object Headers]".to_string(),
            Self::Promise(_) => "[promise]".to_string(),
            Self::Document => "[document]".to_string(),
            Self::Window => "[window]".to_string(),
            Self::Navigator => "[object Navigator]".to_string(),
            Self::Clipboard => "[object Clipboard]".to_string(),
            Self::Location(value) => value.borrow().to_string(),
            Self::ModuleImport(_) => "[object Module]".to_string(),
            Self::History(_) => "[object History]".to_string(),
            Self::Url(value) => value.borrow().url.to_string(),
            Self::UrlSearchParams(value) => serialize_form_urlencoded(&value.borrow().pairs),
            Self::Storage(_) => "[object Storage]".to_string(),
            Self::FormData(_) => "[object FormData]".to_string(),
            Self::Blob(_) => "[object Blob]".to_string(),
            Self::File(_) => "[object File]".to_string(),
            Self::FileList(_) => "[object FileList]".to_string(),
            Self::CanvasContext(_) => "[object CanvasRenderingContext2D]".to_string(),
            Self::AbortController(_) => "[object AbortController]".to_string(),
            Self::AbortSignal(_) => "[object AbortSignal]".to_string(),
            Self::Worker(_) => "[object Worker]".to_string(),
            Self::Console => "[console]".to_string(),
            Self::Json => "[object JSON]".to_string(),
            Self::WebAssemblyNamespace => "[object WebAssembly]".to_string(),
            Self::ResponseConstructor => "[function Response]".to_string(),
            Self::RequestConstructor => "[function Request]".to_string(),
            Self::HeadersConstructor => "[function Headers]".to_string(),
            Self::UrlConstructor | Self::UrlSearchParamsConstructor => {
                "[function URL]".to_string()
            }
            Self::FormDataConstructor => "[function FormData]".to_string(),
            Self::BlobConstructor => "[function Blob]".to_string(),
            Self::FileConstructor => "[function File]".to_string(),
            Self::MapConstructor => "[function Map]".to_string(),
            Self::SetConstructor => "[function Set]".to_string(),
            Self::ErrorConstructor => "[function Error]".to_string(),
            Self::PromiseConstructor => "[function Promise]".to_string(),
            Self::AbortControllerConstructor => "[function AbortController]".to_string(),
            Self::WorkerConstructor => "[function Worker]".to_string(),
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

fn abort_error_value() -> Value {
    Value::Error(Rc::new(ErrorData {
        name: "AbortError".to_string(),
        message: "The operation was aborted".to_string(),
        code: None,
        class_name: "Error".to_string(),
    }))
}

fn abort_event_value() -> Value {
    let mut event = BTreeMap::new();
    event.insert("type".to_string(), Value::String("abort".to_string()));
    event.insert("defaultPrevented".to_string(), Value::Bool(false));
    Value::Object(Rc::new(RefCell::new(event)))
}

fn response_body(bytes: Vec<u8>) -> ResponseBodyHandle {
    Rc::new(RefCell::new(ResponseBodyData {
        bytes,
        cursor: 0,
        consumed: false,
        disturbed: false,
        locked: false,
    }))
}

fn valid_status_text(status_text: &str) -> bool {
    status_text
        .chars()
        .all(|character| character == '\t' || character == ' ' || !character.is_control())
}

fn headers_handle(values: BTreeMap<String, String>, mutable: bool) -> HeadersHandle {
    Rc::new(RefCell::new(HeadersData { values, mutable }))
}

fn request_body_snapshot(body: &ResponseBodyHandle) -> Result<Vec<u8>, ScriptError> {
    let body = body.borrow();
    if body.consumed || body.disturbed || body.locked {
        return Err(runtime_error("request body is already used"));
    }
    Ok(body.bytes[body.cursor..].to_vec())
}

fn clone_request(request: &RequestHandle) -> Result<RequestHandle, ScriptError> {
    Ok(request_handle(
        request.url.clone(),
        request.method.clone(),
        request.headers.borrow().values.clone(),
        request_body_snapshot(&request.body)?,
        request.cache_mode.clone(),
        request.redirect,
        request.signal.clone(),
    ))
}

fn request_handle(
    url: AppUrl,
    method: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    cache_mode: ResourceCacheMode,
    redirect: FetchRedirectMode,
    signal: Option<AbortSignalHandle>,
) -> RequestHandle {
    let body = response_body(body);
    let stream = Rc::new(RefCell::new(ReadableStreamData {
        body: body.clone(),
        canceled: false,
    }));
    Rc::new(RequestData {
        url,
        method,
        headers: headers_handle(headers, true),
        body,
        stream,
        cache_mode,
        redirect,
        signal,
    })
}

fn response_data(
    url: String,
    status: u16,
    status_text: String,
    mime_type: String,
    headers: HeadersHandle,
    bytes: Vec<u8>,
    metadata: ResponseMetadata,
) -> ResponseData {
    let body = response_body(bytes);
    let stream = Rc::new(RefCell::new(ReadableStreamData {
        body: body.clone(),
        canceled: false,
    }));
    ResponseData {
        url,
        status,
        status_text,
        mime_type,
        headers,
        body,
        stream,
        redirected: metadata.redirected,
        response_type: metadata.response_type,
    }
}

fn response_body_used(body: &ResponseBodyHandle) -> bool {
    let body = body.borrow();
    body.consumed || body.disturbed || body.locked
}

fn read_result(value: Value, done: bool) -> Value {
    let mut result = BTreeMap::new();
    result.insert("value".to_string(), value);
    result.insert("done".to_string(), Value::Bool(done));
    Value::Object(Rc::new(RefCell::new(result)))
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

fn body_init_bytes(value: Value) -> Result<(Vec<u8>, Option<String>), ScriptError> {
    match value.resolve() {
        Value::Undefined | Value::Null => Ok((Vec::new(), None)),
        Value::String(body) => Ok((
            body.into_bytes(),
            Some("text/plain;charset=UTF-8".to_string()),
        )),
        Value::Bytes(bytes) => Ok((bytes.as_ref().clone(), None)),
        Value::Blob(blob) => Ok((
            blob.bytes.clone(),
            (!blob.mime_type.is_empty()).then(|| blob.mime_type.clone()),
        )),
        Value::File(file) => Ok((
            file.bytes.clone(),
            (!file.mime_type.is_empty()).then(|| file.mime_type.clone()),
        )),
        Value::UrlSearchParams(params) => Ok((
            serialize_form_urlencoded(&params.borrow().pairs).into_bytes(),
            Some("application/x-www-form-urlencoded;charset=UTF-8".to_string()),
        )),
        Value::FormData(form_data) => {
            let (body, boundary) = multipart_form_data_body(&form_data);
            Ok((body, Some(boundary)))
        }
        _ => Err(runtime_error(
            "body must be a string, ArrayBuffer, Blob, File, URLSearchParams, or FormData",
        )),
    }
}

fn blob_slice_index(
    value: Option<&Value>,
    size: i64,
    default: i64,
) -> Result<i64, ScriptError> {
    let Some(value) = value.map(Value::resolve) else {
        return Ok(default);
    };
    let number = match value {
        Value::Undefined => default as f64,
        Value::Null => 0.0,
        value => value.number()?,
    };
    if number.is_nan() {
        return Ok(0);
    }
    if number == f64::INFINITY {
        return Ok(size);
    }
    if number == f64::NEG_INFINITY {
        return Ok(0);
    }
    let integer = number.trunc() as i64;
    Ok(if integer < 0 {
        (size + integer).max(0)
    } else {
        integer.min(size)
    })
}

fn blob_slice_range(arguments: &[Value], size: i64) -> Result<(usize, usize), ScriptError> {
    let start = blob_slice_index(arguments.first(), size, 0)?;
    let end = blob_slice_index(arguments.get(1), size, size)?;
    let start = start.min(end) as usize;
    let end = end.max(start as i64).min(size) as usize;
    Ok((start, end))
}

fn normalize_blob_type(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| (0x20..=0x7e).contains(&byte))
    {
        value.to_ascii_lowercase()
    } else {
        String::new()
    }
}

fn blob_parts_bytes(parts: Option<&Value>) -> Result<Vec<u8>, ScriptError> {
    let parts = match parts.map(Value::resolve) {
        None | Some(Value::Undefined) | Some(Value::Null) => Vec::new(),
        Some(Value::Array(parts)) => parts.borrow().clone(),
        Some(_) => return Err(runtime_error("Blob parts must be an array")),
    };
    let mut bytes = Vec::new();
    for part in parts {
        match part.resolve() {
            Value::Bytes(part) => bytes.extend_from_slice(part.as_ref()),
            Value::Blob(part) => bytes.extend_from_slice(&part.bytes),
            Value::File(part) => bytes.extend_from_slice(&part.bytes),
            part => bytes.extend_from_slice(part.string_value().as_bytes()),
        }
    }
    Ok(bytes)
}

fn blob_options(
    options: Option<&Value>,
    include_last_modified: bool,
) -> Result<(String, i64), ScriptError> {
    let Some(options) = options.map(Value::resolve) else {
        return Ok((String::new(), 0));
    };
    let options = match options {
        Value::Undefined | Value::Null => return Ok((String::new(), 0)),
        Value::Object(options) => options,
        _ => return Err(runtime_error("Blob options must be an object")),
    };
    let options = options.borrow();
    if options.keys().any(|key| match key.as_str() {
        "type" | "endings" => false,
        "lastModified" => !include_last_modified,
        _ => true,
    }) {
        return Err(runtime_error("Blob options contain an unsupported key"));
    }
    if let Some(endings) = options.get("endings").map(Value::resolve) {
        let valid = matches!(endings, Value::Undefined)
            || matches!(endings, Value::String(value) if value == "transparent");
        if !valid {
            return Err(runtime_error("Blob endings must be transparent"));
        }
    }
    let mime_type = options
        .get("type")
        .map(Value::resolve)
        .and_then(|value| match value {
            Value::Undefined => None,
            value => Some(normalize_blob_type(&value.string_value())),
        })
        .unwrap_or_default();
    let last_modified = if !include_last_modified {
        0
    } else {
        match options.get("lastModified").map(Value::resolve) {
            None | Some(Value::Undefined) => 0,
            Some(Value::Null) => 0,
            Some(value) => {
                let number = value.number()?;
                if !number.is_finite() {
                    return Err(runtime_error("File lastModified must be finite"));
                }
                number.floor().max(0.0) as i64
            }
        }
    };
    Ok((mime_type, last_modified))
}

fn normalize_file_name(value: &str) -> String {
    value
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn validity_object(flags: ConstraintFlags) -> Value {
    let mut values = BTreeMap::new();
    values.insert("badInput".to_string(), Value::Bool(flags.bad_input));
    values.insert("customError".to_string(), Value::Bool(flags.custom_error));
    values.insert(
        "patternMismatch".to_string(),
        Value::Bool(flags.pattern_mismatch),
    );
    values.insert(
        "rangeOverflow".to_string(),
        Value::Bool(flags.range_overflow),
    );
    values.insert(
        "rangeUnderflow".to_string(),
        Value::Bool(flags.range_underflow),
    );
    values.insert("stepMismatch".to_string(), Value::Bool(flags.step_mismatch));
    values.insert("tooLong".to_string(), Value::Bool(flags.too_long));
    values.insert("tooShort".to_string(), Value::Bool(flags.too_short));
    values.insert("typeMismatch".to_string(), Value::Bool(flags.type_mismatch));
    values.insert("valid".to_string(), Value::Bool(flags.valid()));
    values.insert("valueMissing".to_string(), Value::Bool(flags.value_missing));
    Value::Object(Rc::new(RefCell::new(values)))
}

fn valid_email_value(value: &str, multiple: bool) -> bool {
    let values = if multiple {
        value.split(',').map(str::trim).collect::<Vec<_>>()
    } else {
        vec![value]
    };
    !values.is_empty() && values.into_iter().all(valid_email_address)
}

fn valid_email_address(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    if local.is_empty()
        || domain.is_empty()
        || domain.contains('@')
        || local.chars().any(|character| {
            character.is_ascii_whitespace()
                || !character.is_ascii()
                || matches!(character, '"' | '(' | ')' | ',' | ':' | ';' | '<' | '>' | '[' | ']')
        })
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.contains("..")
    {
        return false;
    }
    domain.split('.').all(|label| {
        !label.is_empty()
            && label
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    })
}

fn valid_url_value(value: &str) -> bool {
    let Some((scheme, authority)) = value.split_once("://") else {
        return false;
    };
    !scheme.is_empty()
        && scheme
            .chars()
            .enumerate()
            .all(|(index, character)| {
                if index == 0 {
                    character.is_ascii_alphabetic()
                } else {
                    character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
                }
            })
        && !authority.is_empty()
        && !authority.chars().any(|character| character.is_ascii_whitespace())
}

#[derive(Clone)]
enum PatternAtom {
    Literal(char),
    Any,
    Class(Vec<(char, char)>, bool),
}

#[derive(Clone)]
struct PatternToken {
    atom: PatternAtom,
    minimum: usize,
    maximum: Option<usize>,
}

fn pattern_matches(pattern: &str, value: &str) -> Option<bool> {
    let tokens = parse_pattern_tokens(pattern)?;
    let characters = value.chars().collect::<Vec<_>>();
    let mut memo = HashMap::new();
    Some(pattern_match_from(&tokens, &characters, 0, 0, &mut memo))
}

fn parse_pattern_tokens(pattern: &str) -> Option<Vec<PatternToken>> {
    if pattern.len() > 256 {
        return None;
    }
    let characters = pattern.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while cursor < characters.len() {
        let atom = match characters[cursor] {
            '.' => {
                cursor += 1;
                PatternAtom::Any
            }
            '[' => {
                let (atom, next) = parse_pattern_class(&characters, cursor)?;
                cursor = next;
                atom
            }
            '\\' => {
                let (atom, next) = parse_pattern_escape(&characters, cursor)?;
                cursor = next;
                atom
            }
            '(' | ')' | '|' | '^' | '$' => return None,
            character => {
                cursor += 1;
                PatternAtom::Literal(character)
            }
        };
        let (minimum, maximum, next) = parse_pattern_quantifier(&characters, cursor)?;
        cursor = next;
        tokens.push(PatternToken {
            atom,
            minimum,
            maximum,
        });
    }
    Some(tokens)
}

fn parse_pattern_escape(characters: &[char], cursor: usize) -> Option<(PatternAtom, usize)> {
    let escaped = *characters.get(cursor + 1)?;
    let atom = match escaped {
        'd' => PatternAtom::Class(vec![('0', '9')], false),
        'D' => PatternAtom::Class(vec![('0', '9')], true),
        'w' => PatternAtom::Class(
            vec![('0', '9'), ('A', 'Z'), ('a', 'z'), ('_', '_')],
            false,
        ),
        'W' => PatternAtom::Class(
            vec![('0', '9'), ('A', 'Z'), ('a', 'z'), ('_', '_')],
            true,
        ),
        's' => PatternAtom::Class(
            vec![('\t', '\t'), ('\n', '\n'), ('\r', '\r'), (' ', ' ')],
            false,
        ),
        'S' => PatternAtom::Class(
            vec![('\t', '\t'), ('\n', '\n'), ('\r', '\r'), (' ', ' ')],
            true,
        ),
        character => PatternAtom::Literal(character),
    };
    Some((atom, cursor + 2))
}

fn parse_pattern_class(characters: &[char], cursor: usize) -> Option<(PatternAtom, usize)> {
    let mut cursor = cursor + 1;
    let negated = characters.get(cursor) == Some(&'^');
    if negated {
        cursor += 1;
    }
    let mut ranges = Vec::new();
    while let Some(&character) = characters.get(cursor) {
        if character == ']' {
            return (!ranges.is_empty()).then_some((PatternAtom::Class(ranges, negated), cursor + 1));
        }
        let first = if character == '\\' {
            let escaped = *characters.get(cursor + 1)?;
            cursor += 2;
            match escaped {
                'd' => {
                    ranges.push(('0', '9'));
                    continue;
                }
                'w' => {
                    ranges.extend([('0', '9'), ('A', 'Z'), ('a', 'z'), ('_', '_')]);
                    continue;
                }
                's' => {
                    ranges.extend([('\t', '\t'), ('\n', '\n'), ('\r', '\r'), (' ', ' ')]);
                    continue;
                }
                escaped => escaped,
            }
        } else {
            cursor += 1;
            character
        };
        if characters.get(cursor) == Some(&'-') && characters.get(cursor + 1).is_some_and(|value| *value != ']') {
            let last = characters[cursor + 1];
            ranges.push((first.min(last), first.max(last)));
            cursor += 2;
        } else {
            ranges.push((first, first));
        }
    }
    None
}

fn parse_pattern_quantifier(
    characters: &[char],
    cursor: usize,
) -> Option<(usize, Option<usize>, usize)> {
    let Some(&quantifier) = characters.get(cursor) else {
        return Some((1, Some(1), cursor));
    };
    match quantifier {
        '*' => Some((0, None, cursor + 1)),
        '+' => Some((1, None, cursor + 1)),
        '?' => Some((0, Some(1), cursor + 1)),
        '{' => {
            let mut end = cursor + 1;
            while characters.get(end).is_some_and(|character| character.is_ascii_digit()) {
                end += 1;
            }
            if end == cursor + 1 {
                return None;
            }
            let minimum = characters[cursor + 1..end]
                .iter()
                .collect::<String>()
                .parse::<usize>()
                .ok()?;
            if characters.get(end) == Some(&'}') {
                return Some((minimum, Some(minimum), end + 1));
            }
            if characters.get(end) != Some(&',') {
                return None;
            }
            end += 1;
            let limit_start = end;
            while characters.get(end).is_some_and(|character| character.is_ascii_digit()) {
                end += 1;
            }
            let maximum = if end == limit_start {
                None
            } else {
                Some(
                    characters[limit_start..end]
                        .iter()
                        .collect::<String>()
                        .parse::<usize>()
                        .ok()?,
                )
            };
            if characters.get(end) != Some(&'}') || maximum.is_some_and(|maximum| maximum < minimum) {
                return None;
            }
            Some((minimum, maximum, end + 1))
        }
        _ => Some((1, Some(1), cursor)),
    }
}

fn pattern_match_from(
    tokens: &[PatternToken],
    characters: &[char],
    token_index: usize,
    character_index: usize,
    memo: &mut HashMap<(usize, usize), bool>,
) -> bool {
    if let Some(matched) = memo.get(&(token_index, character_index)) {
        return *matched;
    }
    if token_index == tokens.len() {
        return character_index == characters.len();
    }
    let token = &tokens[token_index];
    let maximum = token
        .maximum
        .unwrap_or(characters.len().saturating_sub(character_index))
        .min(characters.len().saturating_sub(character_index));
    let mut positions = vec![character_index];
    for count in 1..=maximum {
        let position = character_index + count - 1;
        if !pattern_atom_matches(&token.atom, characters[position]) {
            break;
        }
        positions.push(character_index + count);
    }
    let matched = positions
        .iter()
        .enumerate()
        .rev()
        .filter(|(count, _)| *count >= token.minimum)
        .any(|(_, next)| pattern_match_from(tokens, characters, token_index + 1, *next, memo));
    memo.insert((token_index, character_index), matched);
    matched
}

fn pattern_atom_matches(atom: &PatternAtom, character: char) -> bool {
    match atom {
        PatternAtom::Literal(expected) => character == *expected,
        PatternAtom::Any => true,
        PatternAtom::Class(ranges, negated) => {
            let matched = ranges.iter().any(|(start, end)| (*start..=*end).contains(&character));
            if *negated {
                !matched
            } else {
                matched
            }
        }
    }
}

fn truncate_utf16(value: &str, maximum: usize) -> String {
    let mut units = 0usize;
    let mut output = String::new();
    for character in value.chars() {
        let length = character.len_utf16();
        if units.saturating_add(length) > maximum {
            break;
        }
        output.push(character);
        units = units.saturating_add(length);
    }
    output
}

fn utf16_substring(value: &str, start: usize, end: usize) -> String {
    let units = value.encode_utf16().collect::<Vec<_>>();
    let start = start.min(units.len());
    let end = end.min(units.len()).max(start);
    String::from_utf16_lossy(&units[start..end])
}

fn replace_utf16_range(value: &str, start: usize, end: usize, replacement: &str) -> (String, usize) {
    let units = value.encode_utf16().collect::<Vec<_>>();
    let start = start.min(units.len());
    let end = end.min(units.len()).max(start);
    let replacement_units = replacement.encode_utf16().collect::<Vec<_>>();
    let mut next_units = Vec::with_capacity(
        units
            .len()
            .saturating_sub(end.saturating_sub(start))
            .saturating_add(replacement_units.len()),
    );
    next_units.extend_from_slice(&units[..start]);
    next_units.extend_from_slice(&replacement_units);
    next_units.extend_from_slice(&units[end..]);
    (
        String::from_utf16_lossy(&next_units),
        start.saturating_add(replacement_units.len()),
    )
}

fn selection_offset(value: f64) -> usize {
    if value.is_nan() || value <= 0.0 {
        return 0;
    }
    if !value.is_finite() {
        return usize::MAX;
    }
    value.trunc().min(usize::MAX as f64) as usize
}

fn selection_direction(value: &str) -> SelectionDirection {
    match value {
        "forward" => SelectionDirection::Forward,
        "backward" => SelectionDirection::Backward,
        _ => SelectionDirection::None,
    }
}

fn selection_direction_name(direction: SelectionDirection) -> &'static str {
    match direction {
        SelectionDirection::None => "none",
        SelectionDirection::Forward => "forward",
        SelectionDirection::Backward => "backward",
    }
}

fn range_is_collapsed(range: &RangeData) -> bool {
    range.start.path == range.end.path && range.start.offset == range.end.offset
}

fn common_path(start: &[usize], end: &[usize]) -> NodePath {
    start
        .iter()
        .zip(end)
        .take_while(|(left, right)| left == right)
        .map(|(value, _)| *value)
        .collect()
}

fn node_value(path: NodePath) -> Value {
    if path.is_empty() {
        Value::Document
    } else {
        Value::Element(attached_handle(path))
    }
}

fn selection_anchor_focus(selection: TextSelection) -> (usize, usize) {
    match selection.direction {
        SelectionDirection::Backward => (selection.end, selection.start),
        SelectionDirection::Forward | SelectionDirection::None => {
            (selection.start, selection.end)
        }
    }
}

fn previous_utf16_boundary(units: &[u16], offset: usize) -> usize {
    let offset = offset.min(units.len());
    if offset >= 2
        && (0xdc00..=0xdfff).contains(&units[offset - 1])
        && (0xd800..=0xdbff).contains(&units[offset - 2])
    {
        offset - 2
    } else {
        offset.saturating_sub(1)
    }
}

fn next_utf16_boundary(units: &[u16], offset: usize) -> usize {
    let offset = offset.min(units.len());
    if offset + 1 < units.len()
        && (0xd800..=0xdbff).contains(&units[offset])
        && (0xdc00..=0xdfff).contains(&units[offset + 1])
    {
        offset + 2
    } else {
        offset.saturating_add(1).min(units.len())
    }
}

fn preserve_selection_offset(current: usize, start: usize, end: usize, new_end: usize) -> usize {
    if current <= start {
        current
    } else if current <= end {
        new_end
    } else {
        let removed = end.saturating_sub(start);
        let inserted = new_end.saturating_sub(start);
        if inserted >= removed {
            current.saturating_add(inserted - removed)
        } else {
            current.saturating_sub(removed - inserted)
        }
    }
}

fn file_lists_equal(left: &[FileHandle], right: &[FileHandle]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.name == right.name
                && left.mime_type == right.mime_type
                && left.last_modified == right.last_modified
                && left.bytes == right.bytes
        })
}

fn form_data_filename(value: Option<&Value>) -> Option<String> {
    value
        .map(Value::resolve)
        .and_then(|value| match value {
            Value::Undefined => None,
            value => Some(normalize_file_name(&value.string_value())),
        })
}

fn form_data_file(blob: &BlobHandle, name: String) -> FileHandle {
    Rc::new(FileData {
        bytes: blob.bytes.clone(),
        mime_type: blob.mime_type.clone(),
        name,
        last_modified: 0,
    })
}

fn form_data_file_with_name(file: &FileHandle, name: Option<String>) -> FileHandle {
    name.map_or_else(|| file.clone(), |name| {
        Rc::new(FileData {
            bytes: file.bytes.clone(),
            mime_type: file.mime_type.clone(),
            name,
            last_modified: file.last_modified,
        })
    })
}

fn form_data_entry(value: Option<&Value>, filename: Option<String>) -> FormDataEntry {
    match value.map(Value::resolve).unwrap_or(Value::Undefined) {
        Value::Blob(blob) => FormDataEntry::File(form_data_file(
            &blob,
            filename.unwrap_or_else(|| "blob".to_string()),
        )),
        Value::File(file) => FormDataEntry::File(form_data_file_with_name(&file, filename)),
        value => FormDataEntry::Text(value.string_value()),
    }
}

fn form_data_entry_value(entry: &FormDataEntry) -> Value {
    match entry {
        FormDataEntry::Text(value) => Value::String(value.clone()),
        FormDataEntry::File(file) => Value::File(file.clone()),
    }
}

fn form_data_from_body(body: &[u8], mime_type: &str) -> Result<Value, ScriptError> {
    let media_type = mime_type
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let pairs = if media_type == "application/x-www-form-urlencoded" {
        let source = String::from_utf8(body.to_vec())
            .map_err(|_| runtime_error("formData() body is not valid UTF-8"))?;
        parse_form_urlencoded(&source)?
            .into_iter()
            .map(|(name, value)| (name, FormDataEntry::Text(value)))
            .collect()
    } else if media_type == "multipart/form-data" {
        parse_multipart_form_data(body, mime_type)?
    } else {
        return Err(runtime_error(
            "formData() body must be URL-encoded or multipart/form-data",
        ));
    };
    Ok(Value::FormData(Rc::new(RefCell::new(FormDataData { pairs }))))
}

fn parse_multipart_form_data(
    body: &[u8],
    mime_type: &str,
) -> Result<Vec<(String, FormDataEntry)>, ScriptError> {
    let boundary = multipart_boundary(mime_type)
        .ok_or_else(|| runtime_error("multipart/form-data body has no boundary"))?;
    if boundary.is_empty() || boundary.contains('\r') || boundary.contains('\n') {
        return Err(runtime_error("multipart/form-data boundary is invalid"));
    }
    let delimiter = format!("--{boundary}").into_bytes();
    let mut cursor = 0;
    let mut entries = Vec::new();
    loop {
        if body.get(cursor..cursor + delimiter.len()) != Some(delimiter.as_slice()) {
            return Err(runtime_error("multipart/form-data delimiter is missing"));
        }
        cursor += delimiter.len();
        if body.get(cursor..cursor + 2) == Some(b"--") {
            cursor += 2;
            if body.get(cursor..cursor + 2) == Some(b"\r\n") {
                cursor += 2;
            }
            if cursor != body.len() {
                return Err(runtime_error("multipart/form-data has trailing bytes"));
            }
            return Ok(entries);
        }
        if body.get(cursor..cursor + 2) != Some(b"\r\n") {
            return Err(runtime_error("multipart/form-data delimiter is malformed"));
        }
        cursor += 2;
        let header_end = find_bytes(body, b"\r\n\r\n", cursor)
            .ok_or_else(|| runtime_error("multipart/form-data part headers are incomplete"))?;
        let headers = parse_multipart_part_headers(&body[cursor..header_end])?;
        cursor = header_end + 4;
        let next = find_bytes(body, &format!("\r\n--{boundary}").into_bytes(), cursor)
            .ok_or_else(|| runtime_error("multipart/form-data part delimiter is missing"))?;
        let content = &body[cursor..next];
        let (name, filename, content_type) = headers;
        let value = if let Some(filename) = filename {
            FormDataEntry::File(Rc::new(FileData {
                bytes: content.to_vec(),
                mime_type: content_type.unwrap_or_else(|| "text/plain".to_string()),
                name: normalize_file_name(&filename),
                last_modified: 0,
            }))
        } else {
            let text = String::from_utf8(content.to_vec())
                .map_err(|_| runtime_error("multipart/form-data text part is not UTF-8"))?;
            FormDataEntry::Text(text.trim_start_matches('\u{feff}').to_string())
        };
        entries.push((name, value));
        cursor = next + 2;
    }
}

fn multipart_boundary(mime_type: &str) -> Option<String> {
    let mut parameters = split_multipart_parameters(mime_type).into_iter();
    if !parameters
        .next()?
        .trim()
        .eq_ignore_ascii_case("multipart/form-data")
    {
        return None;
    }
    parameters.find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        if !name.trim().eq_ignore_ascii_case("boundary") {
            return None;
        }
        let value = value.trim();
        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            Some(value[1..value.len() - 1].replace("\\\"", "\"").replace("\\\\", "\\"))
        } else {
            Some(value.to_string())
        }
    })
}

fn parse_multipart_part_headers(
    bytes: &[u8],
) -> Result<(String, Option<String>, Option<String>), ScriptError> {
    let source = String::from_utf8(bytes.to_vec())
        .map_err(|_| runtime_error("multipart/form-data headers are not UTF-8"))?;
    let mut disposition = None;
    let mut content_type = None;
    for line in source.split("\r\n") {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| runtime_error("multipart/form-data header is malformed"))?;
        match name.trim().to_ascii_lowercase().as_str() {
            "content-disposition" => disposition = Some(value.trim().to_string()),
            "content-type" => content_type = Some(normalize_blob_type(value.trim())),
            _ => {}
        }
    }
    let disposition = disposition
        .ok_or_else(|| runtime_error("multipart/form-data part has no Content-Disposition"))?;
    let mut parameters = split_multipart_parameters(&disposition).into_iter();
    if !parameters
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("form-data"))
    {
        return Err(runtime_error(
            "multipart/form-data Content-Disposition is not form-data",
        ));
    }
    let mut field_name = None;
    let mut filename = None;
    for parameter in parameters {
        let Some((name, value)) = parameter.split_once('=') else {
            continue;
        };
        let value = parse_multipart_parameter(value.trim());
        match name.trim().to_ascii_lowercase().as_str() {
            "name" => field_name = Some(value),
            "filename" => filename = Some(value),
            _ => {}
        }
    }
    Ok((
        field_name.ok_or_else(|| runtime_error("multipart/form-data part has no name"))?,
        filename,
        content_type,
    ))
}

fn parse_multipart_parameter(value: &str) -> String {
    let value = if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        &value[1..value.len() - 1]
    } else {
        value
    };
    let mut parsed = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            parsed.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            parsed.push(character);
        }
    }
    parsed
        .replace("%0D", "\r")
        .replace("%0A", "\n")
}

fn split_multipart_parameters(value: &str) -> Vec<String> {
    let mut parameters = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' && quoted {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == ';' && !quoted {
            parameters.push(value[start..index].trim().to_string());
            start = index + character.len_utf8();
        }
    }
    parameters.push(value[start..].trim().to_string());
    parameters
}

fn find_bytes(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(start.min(haystack.len()));
    }
    haystack
        .get(start..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| start + offset)
}

fn multipart_form_data_body(form_data: &FormDataHandle) -> (Vec<u8>, String) {
    let pairs = form_data.borrow().pairs.clone();
    let mut boundary = "----ceri-form-data-boundary".to_string();
    while pairs.iter().any(|(name, value)| {
        name.contains(&boundary) || form_data_entry_contains(value, &boundary)
    }) {
        boundary.push('x');
    }
    let mut body = Vec::new();
    for (name, value) in pairs {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match value {
            FormDataEntry::Text(value) => {
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{}\"\r\n\r\n",
                        multipart_parameter(&name)
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(value.as_bytes());
            }
            FormDataEntry::File(file) => {
                let mime_type = if file.mime_type.is_empty() {
                    "application/octet-stream"
                } else {
                    &file.mime_type
                };
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {mime_type}\r\n\r\n",
                        multipart_parameter(&name),
                        multipart_parameter(&file.name)
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(&file.bytes);
            }
        }
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (
        body,
        format!("multipart/form-data; boundary={boundary}"),
    )
}

fn form_data_entry_contains(entry: &FormDataEntry, boundary: &str) -> bool {
    match entry {
        FormDataEntry::Text(value) => value.contains(boundary),
        FormDataEntry::File(file) => {
            file.name.contains(boundary)
                || file.bytes.windows(boundary.len()).any(|window| window == boundary.as_bytes())
        }
    }
}

fn multipart_parameter(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
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
            (Value::Blob(_), Value::BlobConstructor) => true,
            (Value::File(_), Value::FileConstructor | Value::BlobConstructor) => true,
            (Value::Worker(_), Value::WorkerConstructor) => true,
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
        | Value::Validity(_)
        | Value::Range(_)
        | Value::Selection(_)
        | Value::Dataset(_)
        | Value::Style(_)
        | Value::ClassList(_)
        | Value::Event(_)
        | Value::Request(_)
        | Value::Response(_)
        | Value::Bytes(_)
        | Value::ReadableStream(_)
        | Value::ReadableStreamReader(_)
        | Value::Headers(_)
        | Value::Promise(_)
        | Value::Document
        | Value::Window
        | Value::Navigator
        | Value::Clipboard
        | Value::Location(_)
        | Value::ModuleImport(_)
        | Value::History(_)
        | Value::Url(_)
        | Value::UrlSearchParams(_)
        | Value::Storage(_)
        | Value::FormData(_)
        | Value::Blob(_)
        | Value::File(_)
        | Value::FileList(_)
        | Value::CanvasContext(_)
        | Value::AbortController(_)
        | Value::AbortSignal(_)
        | Value::Worker(_)
        | Value::Console
        | Value::Json
        | Value::WebAssemblyNamespace
        | Value::Error(_) => "object",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Symbol(_) => "symbol",
        Value::Binding(_, _) => unreachable!("binding values resolve before typeof"),
        Value::Function(_)
        | Value::Builtin(_)
        | Value::ResponseConstructor
        | Value::RequestConstructor
        | Value::HeadersConstructor
        | Value::UrlConstructor
        | Value::UrlSearchParamsConstructor
        | Value::FormDataConstructor
        | Value::BlobConstructor
        | Value::FileConstructor
        | Value::MapConstructor
        | Value::SetConstructor
        | Value::ErrorConstructor
        | Value::PromiseConstructor
        | Value::AbortControllerConstructor
        | Value::WorkerConstructor
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
        (Value::Validity(left), Value::Validity(right)) => Rc::ptr_eq(left, right),
        (Value::Range(left), Value::Range(right)) => Rc::ptr_eq(left, right),
        (Value::Selection(left), Value::Selection(right)) => Rc::ptr_eq(left, right),
        (Value::Url(left), Value::Url(right)) => Rc::ptr_eq(left, right),
        (Value::UrlSearchParams(left), Value::UrlSearchParams(right)) => Rc::ptr_eq(left, right),
        (Value::Storage(left), Value::Storage(right)) => Rc::ptr_eq(left, right),
        (Value::Navigator, Value::Navigator) | (Value::Clipboard, Value::Clipboard) => true,
        (Value::FormData(left), Value::FormData(right)) => Rc::ptr_eq(left, right),
        (Value::Blob(left), Value::Blob(right)) => Rc::ptr_eq(left, right),
        (Value::File(left), Value::File(right)) => Rc::ptr_eq(left, right),
        (Value::FileList(left), Value::FileList(right)) => Rc::ptr_eq(left, right),
        (Value::CanvasContext(left), Value::CanvasContext(right)) => Rc::ptr_eq(left, right),
        (Value::AbortController(left), Value::AbortController(right)) => Rc::ptr_eq(left, right),
        (Value::AbortSignal(left), Value::AbortSignal(right)) => Rc::ptr_eq(left, right),
        (Value::Worker(left), Value::Worker(right)) => Rc::ptr_eq(left, right),
        (Value::Bytes(left), Value::Bytes(right)) => Rc::ptr_eq(left, right),
        (Value::Request(left), Value::Request(right)) => Rc::ptr_eq(left, right),
        (Value::Headers(left), Value::Headers(right)) => Rc::ptr_eq(left, right),
        (Value::ReadableStream(left), Value::ReadableStream(right)) => Rc::ptr_eq(left, right),
        (Value::ReadableStreamReader(left), Value::ReadableStreamReader(right)) => {
            Rc::ptr_eq(left, right)
        }
        _ => false,
    }
}

fn iterator_value(values: Vec<Value>) -> Value {
    Value::Iterator(Rc::new(RefCell::new(IteratorData { values, index: 0 })))
}

fn normalized_header_name(arguments: &[Value]) -> Result<String, ScriptError> {
    let name = arguments
        .first()
        .map(Value::string_value)
        .unwrap_or_default();
    let mut header = BTreeMap::new();
    header.insert(name, String::new());
    normalize_headers(header)
        .map_err(|error| runtime_error(error.to_string()))?
        .into_iter()
        .next()
        .map(|(name, _)| name)
        .ok_or_else(|| runtime_error("header name is empty"))
}

fn normalized_header_pair(arguments: &[Value]) -> Result<(String, String), ScriptError> {
    let name = arguments
        .first()
        .map(Value::string_value)
        .unwrap_or_default();
    let value = arguments
        .get(1)
        .map(Value::string_value)
        .unwrap_or_else(|| "undefined".to_string());
    let mut header = BTreeMap::new();
    header.insert(name, value);
    normalize_headers(header)
        .map_err(|error| runtime_error(error.to_string()))?
        .into_iter()
        .next()
        .ok_or_else(|| runtime_error("header name is empty"))
}

fn header_values_from_init(init: Value) -> Result<BTreeMap<String, String>, ScriptError> {
    let values = match init.resolve() {
        Value::Undefined | Value::Null => BTreeMap::new(),
        Value::Headers(headers) => headers.borrow().values.clone(),
        Value::Object(object) => object
            .borrow()
            .iter()
            .map(|(name, value)| (name.clone(), value.string_value()))
            .collect(),
        Value::Array(array) => {
            let mut values: BTreeMap<String, String> = BTreeMap::new();
            for entry in array.borrow().iter() {
                let Value::Array(entry) = entry.resolve() else {
                    return Err(runtime_error("Headers sequence entries must be arrays"));
                };
                let entry = entry.borrow();
                if entry.len() != 2 {
                    return Err(runtime_error("Headers sequence entries need two values"));
                }
                let mut pair = BTreeMap::new();
                pair.insert(entry[0].string_value(), entry[1].string_value());
                let (name, value) = normalize_headers(pair)
                    .map_err(|error| runtime_error(error.to_string()))?
                    .into_iter()
                    .next()
                    .ok_or_else(|| runtime_error("header name is empty"))?;
                if let Some(existing) = values.get_mut(&name) {
                    existing.push_str(", ");
                    existing.push_str(&value);
                } else {
                    values.insert(name, value);
                }
            }
            values
        }
        _ => {
            return Err(runtime_error(
                "Headers constructor requires an object, sequence, or Headers",
            ));
        }
    };
    normalize_headers(values).map_err(|error| runtime_error(error.to_string()))
}

fn parse_cache_mode(
    value: Option<&Value>,
    default: ResourceCacheMode,
) -> Result<ResourceCacheMode, ScriptError> {
    match value.map(Value::resolve) {
        None | Some(Value::Undefined) => Ok(default),
        Some(Value::String(mode)) => match mode.as_str() {
            "default" => Ok(ResourceCacheMode::Default),
            "no-store" => Ok(ResourceCacheMode::NoStore),
            "reload" => Ok(ResourceCacheMode::Reload),
            "no-cache" => Ok(ResourceCacheMode::NoCache),
            "force-cache" => Ok(ResourceCacheMode::ForceCache),
            "only-if-cached" => Ok(ResourceCacheMode::OnlyIfCached),
            _ => Err(runtime_error(format!("unsupported fetch cache mode {mode}"))),
        },
        Some(_) => Err(runtime_error("fetch cache mode must be a string")),
    }
}

fn parse_redirect_mode(
    value: Option<&Value>,
    default: FetchRedirectMode,
) -> Result<FetchRedirectMode, ScriptError> {
    match value.map(Value::resolve) {
        None | Some(Value::Undefined) => Ok(default),
        Some(Value::String(mode)) => match mode.as_str() {
            "follow" => Ok(FetchRedirectMode::Follow),
            "error" => Ok(FetchRedirectMode::Error),
            "manual" => Ok(FetchRedirectMode::Manual),
            _ => Err(runtime_error(format!("unsupported fetch redirect mode {mode}"))),
        },
        Some(_) => Err(runtime_error("fetch redirect mode must be a string")),
    }
}

fn parse_signal(
    value: Option<&Value>,
    default: Option<AbortSignalHandle>,
) -> Result<Option<AbortSignalHandle>, ScriptError> {
    match value.map(Value::resolve) {
        None | Some(Value::Undefined) => Ok(default),
        Some(Value::Null) => Ok(None),
        Some(Value::AbortSignal(signal)) => Ok(Some(signal)),
        Some(_) => Err(runtime_error("fetch signal must be an AbortSignal")),
    }
}

fn fetch_cache_mode_name(mode: &ResourceCacheMode) -> String {
    match mode {
        ResourceCacheMode::Default => "default",
        ResourceCacheMode::NoStore => "no-store",
        ResourceCacheMode::Reload => "reload",
        ResourceCacheMode::NoCache => "no-cache",
        ResourceCacheMode::ForceCache => "force-cache",
        ResourceCacheMode::OnlyIfCached => "only-if-cached",
    }
    .to_string()
}

fn fetch_redirect_mode_name(mode: FetchRedirectMode) -> String {
    match mode {
        FetchRedirectMode::Follow => "follow",
        FetchRedirectMode::Error => "error",
        FetchRedirectMode::Manual => "manual",
    }
    .to_string()
}

const MAX_USER_ITERATOR_VALUES: usize = 10_000;

struct FetchOptions {
    method: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    body_from_request: bool,
    cache_mode: ResourceCacheMode,
    redirect: FetchRedirectMode,
    signal: Option<AbortSignalHandle>,
}

#[derive(Clone, Copy)]
enum FetchRedirectMode {
    Follow,
    Error,
    Manual,
}

fn parse_fetch_options(
    options: Option<&Value>,
    request_input: Option<&RequestHandle>,
) -> Result<FetchOptions, ScriptError> {
    let default_method = request_input
        .map(|request| request.method.clone())
        .unwrap_or_else(|| "GET".to_string());
    let default_headers = request_input
        .map(|request| request.headers.borrow().values.clone())
        .unwrap_or_default();
    let default_cache = request_input
        .map(|request| request.cache_mode.clone())
        .unwrap_or(ResourceCacheMode::Default);
    let default_redirect = request_input
        .map(|request| request.redirect)
        .unwrap_or(FetchRedirectMode::Follow);
    let default_signal = request_input.and_then(|request| request.signal.clone());
    let Some(options) = options.map(Value::resolve) else {
        return Ok(FetchOptions {
            method: default_method,
            headers: default_headers,
            body: request_input
                .map(|request| request_body_snapshot(&request.body))
                .transpose()?
                .unwrap_or_default(),
            body_from_request: request_input.is_some(),
            cache_mode: default_cache,
            redirect: default_redirect,
            signal: default_signal,
        });
    };
    if matches!(options, Value::Undefined | Value::Null) {
        return Ok(FetchOptions {
            method: default_method,
            headers: default_headers,
            body: request_input
                .map(|request| request_body_snapshot(&request.body))
                .transpose()?
                .unwrap_or_default(),
            body_from_request: request_input.is_some(),
            cache_mode: default_cache,
            redirect: default_redirect,
            signal: default_signal,
        });
    }
    let Value::Object(options) = options else {
        return Err(runtime_error("fetch options must be an object"));
    };
    let options = options.borrow();
    if options.keys().any(|key| {
        !matches!(
            key.as_str(),
            "method" | "headers" | "body" | "cache" | "redirect" | "signal"
        )
    }) {
        return Err(runtime_error(
            "fetch options contain an unsupported key",
        ));
    }
    let method = match options.get("method").map(Value::resolve) {
        None | Some(Value::Undefined) => default_method,
        Some(Value::String(method)) => method,
        Some(_) => return Err(runtime_error("fetch method must be a string")),
    };
    let headers = match options.get("headers").map(Value::resolve) {
        None | Some(Value::Undefined) => default_headers,
        Some(Value::Null) => BTreeMap::new(),
        Some(value) => header_values_from_init(value)?,
    };
    let (body, inferred_mime_type) = match options.get("body").map(Value::resolve) {
        None | Some(Value::Undefined) => (
            request_input
                .map(|request| request_body_snapshot(&request.body))
                .transpose()?
                .unwrap_or_default(),
            None,
        ),
        Some(value) => body_init_bytes(value)?,
    };
    let mut headers = headers;
    if let Some(mime_type) = inferred_mime_type
        && !headers.contains_key("content-type")
    {
        headers.insert("content-type".to_string(), mime_type);
    }
    let body_from_request = request_input.is_some()
        && matches!(options.get("body").map(Value::resolve), None | Some(Value::Undefined));
    let cache_mode = parse_cache_mode(options.get("cache"), default_cache)?;
    let redirect = parse_redirect_mode(options.get("redirect"), default_redirect)?;
    let signal = parse_signal(options.get("signal"), default_signal)?;
    Ok(FetchOptions {
        method,
        headers,
        body,
        body_from_request,
        cache_mode,
        redirect,
        signal,
    })
}

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
            Value::UrlSearchParams(params) => Ok(params
                .borrow()
                .pairs
                .iter()
                .map(|(name, value)| {
                    Value::Array(Rc::new(RefCell::new(vec![
                        Value::String(name.clone()),
                        Value::String(value.clone()),
                    ])))
                })
                .collect()),
            Value::FormData(form_data) => Ok(form_data
                .borrow()
                .pairs
                .iter()
                .map(|(name, value)| {
                    Value::Array(Rc::new(RefCell::new(vec![
                        Value::String(name.clone()),
                        form_data_entry_value(value),
                    ])))
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
    use crate::navigation::NavigationError;

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

    struct RedirectFetchProvider {
        requests: Rc<RefCell<Vec<ResourceRequest>>>,
    }

    impl ResourceProvider for RedirectFetchProvider {
        fn provide(&self, _url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
            Ok(None)
        }

        fn provide_response(
            &self,
            request: &ResourceRequest,
        ) -> Result<Option<ResourceResponse>, NavigationError> {
            self.requests.borrow_mut().push(request.clone());
            match request.url().path() {
                "/redirect" => {
                    let mut headers = BTreeMap::new();
                    headers.insert("Location".to_string(), "/final".to_string());
                    ResourceResponse::new(302, "text/plain", headers, Vec::new()).map(Some)
                }
                "/final" => ResourceResponse::new(
                    200,
                    "text/plain",
                    BTreeMap::new(),
                    b"followed".to_vec(),
                )
                .map(Some),
                "/submit" => {
                    let mut headers = BTreeMap::new();
                    headers.insert("Location".to_string(), "/done".to_string());
                    ResourceResponse::new(303, "text/plain", headers, Vec::new()).map(Some)
                }
                "/done" => ResourceResponse::new(
                    200,
                    "text/plain",
                    BTreeMap::new(),
                    b"submitted".to_vec(),
                )
                .map(Some),
                _ => Ok(None),
            }
        }
    }

    struct HeadersFetchProvider {
        requests: Rc<RefCell<Vec<ResourceRequest>>>,
    }

    impl ResourceProvider for HeadersFetchProvider {
        fn provide(&self, _url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
            Ok(None)
        }

        fn provide_response(
            &self,
            request: &ResourceRequest,
        ) -> Result<Option<ResourceResponse>, NavigationError> {
            self.requests.borrow_mut().push(request.clone());
            if request.url().path() == "/form" {
                let mut headers = BTreeMap::new();
                headers.insert(
                    "Content-Type".to_string(),
                    "application/x-www-form-urlencoded;charset=UTF-8".to_string(),
                );
                return ResourceResponse::new(
                    200,
                    "application/x-www-form-urlencoded;charset=UTF-8",
                    headers,
                    b"answer=42&answer=second+value".to_vec(),
                )
                .map(Some);
            }
            if request.url().path() == "/echo-form" {
                let mime_type = request
                    .headers()
                    .get("content-type")
                    .cloned()
                    .unwrap_or_default();
                let mut headers = BTreeMap::new();
                headers.insert("Content-Type".to_string(), mime_type.clone());
                return ResourceResponse::new(
                    200,
                    mime_type,
                    headers,
                    request.body().to_vec(),
                )
                .map(Some);
            }
            if request.url().path() != "/headers" {
                return Ok(None);
            }
            let mut headers = BTreeMap::new();
            headers.insert("X-Zed".to_string(), "last".to_string());
            headers.insert("Content-Type".to_string(), "text/plain".to_string());
            headers.insert("X-Alpha".to_string(), "first".to_string());
            ResourceResponse::new(200, "text/plain", headers, b"ready".to_vec()).map(Some)
        }
    }

    struct WorkerScriptProvider {
        requests: Rc<RefCell<Vec<String>>>,
    }

    impl ResourceProvider for WorkerScriptProvider {
        fn provide(&self, url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
            self.requests.borrow_mut().push(url.path().to_string());
            Ok((url.path() == "/worker.js").then(|| {
                Resource::new(
                    "text/javascript",
                    br#"self.onmessage = (event) => { postMessage(event.data + "!"); };"#
                        .to_vec(),
                )
            }))
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
            .key_down_with_modifiers("Backspace", KeyModifiers::default())
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
    fn autofocus_skips_invalid_candidates_and_preserves_existing_focus() {
        let first_document = document(
            r#"<div autofocus>ignored</div><input id="disabled" autofocus disabled><input id="first" autofocus><button id="second" autofocus>second</button>"#,
        );
        let mut runtime = ScriptRuntime::new(first_document.clone());
        assert!(runtime
            .apply_autofocus()
            .expect("autofocus should focus the first valid candidate"));
        assert_eq!(runtime.active_element(), Some(vec![2]));
        assert!(!runtime
            .apply_autofocus()
            .expect("autofocus should not replace existing focus"));

        let manual_document = document(
            r#"<input id="first" autofocus><button id="second">second</button>"#,
        );
        let mut manual_runtime = ScriptRuntime::new(manual_document.clone());
        manual_runtime
            .execute("document.getElementById(\"second\").focus();")
            .expect("script focus should execute");
        assert_eq!(manual_runtime.active_element(), Some(vec![1]));
        assert!(!manual_runtime
            .apply_autofocus()
            .expect("autofocus should preserve script focus"));
        assert_eq!(manual_runtime.active_element(), Some(vec![1]));
    }

    #[test]
    fn readonly_text_controls_keep_focus_and_selection_but_block_user_edits() {
        let document = document(
            r#"<input id="code" value="rust" readonly><textarea id="note" readonly>draft</textarea><p id="status">idle</p>"#,
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const code = document.getElementById("code");
                document.getElementById("status").textContent = code.readOnly + "|" + code.value;
                "#,
            )
            .expect("readonly property should be readable");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("true|rust")
        );
        let code_path = document
            .borrow()
            .find_element_by_id("code")
            .expect("code input should exist");
        assert!(runtime
            .focus_path(code_path.clone())
            .expect("readonly input should remain focusable"));
        assert!(!runtime
            .insert_text("!")
            .expect("readonly input should reject host text"));
        assert!(!runtime
            .key_down_with_modifiers("Backspace", KeyModifiers::default())
            .expect("readonly input should reject deletion"));
        assert!(runtime
            .key_down_with_modifiers("ArrowLeft", KeyModifiers::default())
            .expect("readonly input should allow selection movement"));
        assert_eq!(runtime.active_element(), Some(vec![0]));
        runtime
            .execute("document.getElementById(\"status\").textContent = document.getElementById(\"code\").selectionStart + \"|\" + document.getElementById(\"code\").selectionEnd;")
            .expect("readonly selection should remain script-visible");
        assert!(!runtime
            .composition_update("x", None)
            .expect("readonly input should reject composition"));
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("3|3")
        );
        let note_path = document
            .borrow()
            .find_element_by_id("note")
            .expect("readonly textarea should exist");
        assert!(runtime
            .focus_path(note_path)
            .expect("readonly textarea should remain focusable"));
        assert!(!runtime
            .key_down_with_modifiers("Enter", KeyModifiers::default())
            .expect("readonly textarea should reject Enter editing"));
        runtime
            .execute("document.getElementById(\"status\").textContent = document.getElementById(\"note\").value;")
            .expect("readonly textarea value should remain script-visible");
        assert_eq!(document.borrow().element_text_content(&status).as_deref(), Some("draft"));
        assert!(runtime
            .focus_path(code_path)
            .expect("code input should regain focus"));
        runtime
            .execute("document.getElementById(\"code\").readOnly = false; document.getElementById(\"code\").value = \"script\";")
            .expect("script should be able to change readonly state and value");
        assert!(runtime.insert_text("!").expect("editable value should accept text"));
        runtime
            .execute("document.getElementById(\"status\").textContent = document.getElementById(\"code\").value;")
            .expect("script should read the updated value");
        assert_eq!(document.borrow().element_text_content(&status).as_deref(), Some("scr!ipt"));
    }

    #[test]
    fn maxlength_caps_host_text_and_composition_by_utf16_units() {
        let document = document(
            r#"<input id="code" maxlength="4" value="ab"><input id="ime" maxlength="5" value="abc"><textarea id="note" maxlength="4">abc</textarea><p id="status">idle</p>"#,
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        runtime
            .execute(
                r#"
                const code = document.getElementById("code");
                const status = document.getElementById("status");
                code.addEventListener("input", (event) => status.textContent = event.data + "|" + code.value);
                code.focus();
                "#,
            )
            .expect("maxlength listeners should register");
        assert!(runtime
            .insert_text("😀x")
            .expect("host text should insert up to maxlength"));
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("😀|ab😀")
        );
        runtime
            .execute("code.value = \"script-long\"; status.textContent = code.value;")
            .expect("script value writes should remain unrestricted");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("script-long")
        );
        runtime
            .execute("status.textContent = code.value + \"|\" + code.validity.tooLong;")
            .expect("script value validity should remain separate from user edits");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("script-long|false")
        );

        let ime_path = document
            .borrow()
            .find_element_by_id("ime")
            .expect("IME input should exist");
        assert!(runtime.focus_path(ime_path.clone()).expect("IME input should focus"));
        assert!(runtime
            .composition_update("😀x", None)
            .expect("composition should fit whole UTF-16 characters"));
        assert_eq!(runtime.control_value_path(&ime_path).as_deref(), Some("abc😀"));
        assert!(runtime
            .composition_commit("xy")
            .expect("composition should commit within maxlength"));
        assert_eq!(runtime.control_value_path(&ime_path).as_deref(), Some("abcxy"));

        let note_path = document
            .borrow()
            .find_element_by_id("note")
            .expect("textarea should exist");
        assert!(runtime.focus_path(note_path.clone()).expect("textarea should focus"));
        assert!(runtime
            .key_down_with_modifiers("Enter", KeyModifiers::default())
            .expect("Enter should use the last available UTF-16 unit"));
        assert_eq!(runtime.control_value_path(&note_path).as_deref(), Some("abc\n"));
        assert!(!runtime
            .key_down_with_modifiers("Enter", KeyModifiers::default())
            .expect("Enter should stop at maxlength"));
        assert_eq!(runtime.control_value_path(&note_path).as_deref(), Some("abc\n"));
    }

    #[test]
    fn constraint_validation_tracks_user_lengths_and_gates_form_submission() {
        let document = document(
            r#"<form id="signup"><input id="name" required minlength="3"><button id="submit" type="submit">send</button></form><p id="status">idle</p>"#,
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const form = document.getElementById("signup");
                const name = document.getElementById("name");
                const status = document.getElementById("status");
                const validity = name.validity;
                let invalid = 0;
                let submits = 0;
                name.addEventListener("invalid", () => invalid = invalid + 1);
                form.addEventListener("submit", (event) => { submits = submits + 1; event.preventDefault(); });
                status.textContent = name.willValidate + ":" + name.validity.valueMissing + ":" + name.checkValidity() + ":" + name.validationMessage + ":" + form.checkValidity() + ":" + invalid;
                form.requestSubmit();
                "#,
            )
            .expect("constraint validation should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("true:true:false:value missing:false:2")
        );

        let name_path = document
            .borrow()
            .find_element_by_id("name")
            .expect("name input should exist");
        assert!(runtime.focus_path(name_path).expect("name input should focus"));
        assert!(runtime.insert_text("x").expect("short user value should insert"));
        runtime
            .execute("status.textContent = name.value + \"|\" + validity.tooShort + \"|\" + name.checkValidity() + \"|\" + form.checkValidity() + \"|\" + submits;")
            .expect("short value validity should be readable");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("x|true|false|false|0")
        );
        assert!(runtime.insert_text("yz").expect("valid user value should insert"));
        runtime
            .execute("name.setCustomValidity(\"custom\"); status.textContent = validity.customError + \"|\" + name.validationMessage + \"|\" + name.checkValidity() + \"|\" + submits; name.setCustomValidity(\"\"); form.requestSubmit(); status.textContent = name.value + \"|\" + validity.valid + \"|\" + submits;")
            .expect("custom validity and valid submission should execute");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("xyz|true|1")
        );
    }

    #[test]
    fn email_url_and_bounded_pattern_constraints_report_type_mismatches() {
        let document = document(
            r#"<input id="email" type="email" value="bad address"><input id="url" type="url" value="example"><input id="code" pattern="[0-9]{4}" value="12"><input id="ignored" pattern="(" value="x"><p id="status">idle</p>"#,
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const email = document.getElementById("email");
                const url = document.getElementById("url");
                const code = document.getElementById("code");
                const ignored = document.getElementById("ignored");
                document.getElementById("status").textContent = email.validity.typeMismatch + "|" + url.validity.typeMismatch + "|" + code.validity.patternMismatch + "|" + ignored.validity.valid + "|" + email.validationMessage;
                "#,
            )
            .expect("typed constraints should execute");
        let mut status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("true|true|true|true|type mismatch")
        );
        runtime
            .execute("email.value = \"person@example.com\"; url.value = \"https://example.com/account\"; code.value = \"1234\"; document.getElementById(\"status\").textContent = email.validity.valid + \"|\" + url.validity.valid + \"|\" + code.validity.valid;")
            .expect("valid typed values should execute");
        status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("true|true|true")
        );
    }

    #[test]
    fn tab_navigation_moves_focus_in_document_order_and_honors_positive_tabindex() {
        let document = document(
            r#"<input id="first"><button id="second">second</button><textarea id="third"></textarea><input id="priority" tabindex="2"><input id="earlier" tabindex="1"><input id="skip" tabindex="-1"><p id="status"></p>"#,
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const first = document.getElementById("first");
                const second = document.getElementById("second");
                let events = "";
                first.addEventListener("blur", () => events = events + "first-blur|");
                second.addEventListener("focus", () => events = events + "second-focus|");
                "#,
            )
            .expect("focus listeners should register");
        let first_path = document
            .borrow()
            .find_element_by_id("first")
            .expect("first input should exist");
        assert!(runtime.focus_path(first_path).expect("first input should focus"));
        assert!(runtime
            .key_down_with_modifiers("Tab", KeyModifiers::default())
            .expect("Tab should move focus"));
        assert_eq!(runtime.active_element(), Some(vec![1]));
        assert_eq!(
            runtime
                .execute("document.getElementById(\"status\").textContent = events;"),
            Ok(())
        );
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(document.borrow().element_text_content(&status).as_deref(), Some("first-blur|second-focus|"));
        assert!(runtime
            .key_down_with_modifiers(
                "Tab",
                KeyModifiers {
                    shift: true,
                    ..KeyModifiers::default()
                },
            )
            .expect("Shift+Tab should move focus backward"));
        assert_eq!(runtime.active_element(), Some(vec![0]));
        assert!(runtime
            .key_down_with_modifiers(
                "Tab",
                KeyModifiers {
                    shift: true,
                    ..KeyModifiers::default()
                },
            )
            .expect("Shift+Tab should reach positive tabindex entries"));
        assert_eq!(runtime.active_element(), Some(vec![3]));
        assert!(runtime
            .key_down_with_modifiers(
                "Tab",
                KeyModifiers {
                    shift: true,
                    ..KeyModifiers::default()
                },
            )
            .expect("Shift+Tab should follow positive tabindex ordering"));
        assert_eq!(runtime.active_element(), Some(vec![4]));
        let skipped_path = document
            .borrow()
            .find_element_by_id("skip")
            .expect("negative tabindex input should exist");
        assert_ne!(runtime.active_element(), Some(skipped_path));
    }

    #[test]
    fn text_control_selection_apis_edit_ranges_and_report_direction() {
        let document = document(
            r#"<input id="name" value="hello"><textarea id="note">world</textarea><input id="amount" type="number" value="12"><p id="status">idle</p>"#,
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const input = document.getElementById("name");
                const note = document.getElementById("note");
                const amount = document.getElementById("amount");
                let selects = 0;
                let selectionChanges = 0;
                input.addEventListener("select", () => selects = selects + 1);
                input.addEventListener("selectionchange", () => selectionChanges = selectionChanges + 1);
                input.setSelectionRange(1, 4, "backward");
                const before = input.selectionStart + ":" + input.selectionEnd + ":" + input.selectionDirection;
                input.setRangeText("i", input.selectionStart, input.selectionEnd, "end");
                note.select();
                note.setRangeText("hi", 0, 5, "select");
                document.getElementById("status").textContent = before + "|" + input.value + ":" + input.selectionStart + ":" + input.selectionEnd + ":" + input.selectionDirection + "|" + note.value + ":" + note.selectionStart + ":" + note.selectionEnd + "|" + amount.selectionStart + "|" + selects + ":" + selectionChanges;
                "#,
            )
            .expect("selection APIs should execute");
        let mut status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("1:4:backward|hio:2:2:none|hi:0:2|null|2:2")
        );

        let input_path = document
            .borrow()
            .find_element_by_id("name")
            .expect("input should exist");
        assert!(runtime
            .focus_path(input_path)
            .expect("input should focus"));
        runtime
            .execute("document.getElementById(\"name\").setSelectionRange(1, 2);")
            .expect("selection should update");
        assert!(runtime.insert_text("X").expect("selection should be replaced"));
        assert_eq!(
            runtime.key_down_with_modifiers("ArrowLeft", KeyModifiers::default()),
            Ok(true)
        );
        assert_eq!(
            runtime.key_down_with_modifiers("Delete", KeyModifiers::default()),
            Ok(true)
        );
        assert_eq!(
            runtime.key_down_with_modifiers("Home", KeyModifiers::default()),
            Ok(true)
        );
        assert!(runtime.insert_text("!").expect("text should insert at home"));
        assert_eq!(runtime.execute("document.getElementById(\"status\").textContent = document.getElementById(\"name\").value;"), Ok(()));
        status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("!ho")
        );
    }

    #[test]
    fn textarea_enter_and_delete_edit_the_live_selection() {
        let document = document(r#"<textarea id="note">AB</textarea>"#);
        let mut runtime = ScriptRuntime::new(document.clone());
        let note_path = document
            .borrow()
            .find_element_by_id("note")
            .expect("textarea should exist");
        assert!(runtime.focus_path(note_path).expect("textarea should focus"));
        assert!(runtime
            .key_down_with_modifiers("Enter", KeyModifiers::default())
            .expect("Enter should insert a line break"));
        assert_eq!(runtime.control_value_path(&[0]).as_deref(), Some("AB\n"));
        assert!(runtime
            .key_down_with_modifiers("Backspace", KeyModifiers::default())
            .expect("Backspace should remove the line break"));
        assert_eq!(runtime.control_value_path(&[0]).as_deref(), Some("AB"));
        assert!(runtime
            .key_down_with_modifiers("ArrowLeft", KeyModifiers::default())
            .expect("ArrowLeft should move before the final character"));
        assert!(runtime
            .key_down_with_modifiers("Delete", KeyModifiers::default())
            .expect("Delete should remove the final character"));
        assert_eq!(runtime.control_value_path(&[0]).as_deref(), Some("A"));
    }

    #[test]
    fn text_control_shift_navigation_extends_utf16_selection_and_exposes_modifiers() {
        let document = document(r#"<input id="name" value="A😀B"><p id="status">idle</p>"#);
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const input = document.getElementById("name");
                const status = document.getElementById("status");
                let flags = "";
                input.addEventListener("keydown", (event) => {
                    flags = event.shiftKey + ":" + event.ctrlKey + ":" + event.altKey + ":" + event.metaKey;
                });
                input.focus();
                input.setSelectionRange(3, 3);
                "#,
            )
            .expect("modifier listener should register");
        assert!(runtime
            .key_down_with_modifiers(
                "ArrowLeft",
                KeyModifiers {
                    shift: true,
                    control: true,
                    alt: false,
                    meta: true,
                },
            )
            .expect("shift arrow should extend selection"));
        runtime
            .execute(
                "status.textContent = input.selectionStart + \"|\" + input.selectionEnd + \"|\" + input.selectionDirection + \"|\" + flags;",
            )
            .expect("selection should be readable");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("1|3|backward|true:true:false:true")
        );
        assert!(runtime
            .key_down_with_modifiers(
                "ArrowRight",
                KeyModifiers {
                    shift: true,
                    ..KeyModifiers::default()
                },
            )
            .expect("shift arrow should contract selection"));
        assert_eq!(
            runtime.execute(
                "status.textContent = input.selectionStart + \"|\" + input.selectionEnd + \"|\" + input.selectionDirection;",
            ),
            Ok(())
        );
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("3|3|none")
        );
    }

    #[test]
    fn document_range_and_selection_cover_bounded_element_text() {
        let document = document(r#"<p id="copy">hello</p><p id="status">idle</p>"#);
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const copy = document.getElementById("copy");
                const status = document.getElementById("status");
                const range = document.createRange();
                let changes = 0;
                document.addEventListener("selectionchange", () => changes = changes + 1);
                range.selectNodeContents(copy);
                const selection = window.getSelection();
                selection.removeAllRanges();
                selection.addRange(range);
                const clone = range.cloneRange();
                clone.setStart(copy, 1);
                clone.setEnd(copy, 4);
                status.textContent = range.toString() + "|" + selection.toString() + "|" + clone.toString() + "|" + range.startOffset + ":" + range.endOffset + ":" + range.collapsed + ":" + range.commonAncestorContainer.id + "|" + selection.rangeCount + ":" + selection.anchorNode.id + ":" + selection.anchorOffset + ":" + selection.focusOffset;
                selection.collapse(copy, 2);
                status.textContent = status.textContent + "|" + selection.isCollapsed + ":" + selection.toString() + ":" + selection.rangeCount + ":" + changes;
                "#,
            )
            .expect("Range and Selection should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("hello|hello|ell|0:5:false:copy|1:copy:0:5|true::1:2")
        );
    }

    #[test]
    fn text_control_selection_uses_utf16_offsets() {
        let document = document(r#"<input id="emoji" value="A😀B"><p id="status">idle</p>"#);
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const input = document.getElementById("emoji");
                input.setSelectionRange(1, 3);
                input.setRangeText("x");
                document.getElementById("status").textContent = input.value + "|" + input.selectionStart + "|" + input.selectionEnd;
                "#,
            )
            .expect("UTF-16 selection should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("AxB|1|2")
        );
    }

    #[test]
    fn text_composition_replaces_preedit_and_commits_or_cancels() {
        let document = document(r#"<input id="name" value="ab"><p id="status">idle</p>"#);
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const input = document.getElementById("name");
                let events = "";
                input.addEventListener("compositionstart", (event) => events = events + event.type + ":" + event.data + "|");
                input.addEventListener("compositionupdate", (event) => events = events + event.type + ":" + event.data + ":" + input.value + ":" + event.isComposing + "|");
                input.addEventListener("input", (event) => events = events + event.data + ":" + input.value + ":" + event.isComposing + "|");
                input.addEventListener("compositionend", (event) => events = events + event.type + ":" + event.data + ":" + input.value + "|");
                input.focus();
                input.setSelectionRange(1, 1);
                "#,
            )
            .expect("composition listeners should register");
        assert!(runtime
            .composition_update("x", Some((1, 1)))
            .expect("preedit should update"));
        assert!(runtime
            .composition_update("xy", Some((2, 2)))
            .expect("preedit should replace"));
        assert!(runtime
            .composition_commit("安")
            .expect("composition should commit"));
        runtime
            .execute(
                "document.getElementById(\"status\").textContent = document.getElementById(\"name\").value + \"|\" + document.getElementById(\"name\").selectionStart + \"|\" + events;",
            )
            .expect("composition result should be readable");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("a安b|2|compositionstart:|compositionupdate:x:ab:true|x:axb:true|compositionupdate:xy:axb:true|xy:axyb:true|安:a安b:true|compositionend:安:a安b|")
        );

        let cancel_document = Rc::new(RefCell::new(
            Document::parse(r#"<input id="name" value="ab"><p id="status">idle</p>"#)
                .expect("cancel document should parse"),
        ));
        let mut cancel_runtime = ScriptRuntime::new(cancel_document.clone());
        cancel_runtime
            .execute(
                "const input = document.getElementById(\"name\"); input.focus(); input.setSelectionRange(1, 1);",
            )
            .expect("canceled composition should prepare");
        assert!(cancel_runtime
            .composition_update("z", Some((1, 1)))
            .expect("second preedit should update"));
        assert!(cancel_runtime
            .composition_cancel()
            .expect("composition should cancel"));
        cancel_runtime
            .execute("document.getElementById(\"status\").textContent = document.getElementById(\"name\").value;")
            .expect("canceled composition should be readable");
        let status_path = cancel_document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            cancel_document
                .borrow()
                .element_text_content(&status_path)
                .as_deref(),
            Some("ab")
        );
    }

    #[test]
    fn element_scroll_offsets_are_readable_and_writable() {
        let document = document(
            "<div id='viewport'><p id='status'>idle</p></div>",
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const viewport = document.getElementById("viewport");
                viewport.addEventListener("scroll", () => {
                    document.getElementById("status").textContent = "event:" + viewport.scrollTop;
                });
                viewport.scrollLeft = 12;
                viewport.scrollTop = 34;
                document.getElementById("status").textContent = viewport.scrollLeft + ":" + viewport.scrollTop + ":" + document.getElementById("status").textContent;
                "#,
            )
            .expect("scroll properties should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("12:34:event:34")
        );
    }

    #[test]
    fn window_scroll_apis_update_offsets_and_dispatch_scroll_events() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                window.addEventListener("scroll", () => {
                    status.textContent = "event:" + window.scrollY;
                });
                window.scrollTo(12, 34);
                const first = window.scrollX + ":" + window.scrollY;
                window.scrollBy({ left: 3, top: 4 });
                status.textContent = first + "|" + window.pageXOffset + ":" + window.pageYOffset + "|" + status.textContent;
                "#,
            )
            .expect("window scroll APIs should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("12:34|15:38|event:38")
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
    fn url_search_params_iteration_preserves_duplicates_and_url_linkage() {
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
                const url = new URL("/search?mode=one&mode=two&tag=rust");
                const params = url.searchParams;
                const keys = params.keys();
                const firstKey = keys.next().value;
                const secondKey = keys.next().value;
                const values = params.values();
                const firstValue = values.next().value;
                const secondValue = values.next().value;
                const firstEntry = params.entries().next().value;
                const symbolEntry = params[Symbol.iterator]().next().value;
                const spread = [...params];
                let seen = "";
                params.forEach((value, name) => { seen = seen + name + "=" + value + ";"; });
                params.append("mode", "three");
                const appended = url.search;
                params.sort();
                const sorted = url.search;
                document.getElementById("status").textContent =
                  firstKey + "|" + secondKey + "|" + firstValue + "|" + secondValue + "|" +
                  firstEntry[0] + "|" + firstEntry[1] + "|" + seen + "|" + spread.length + "|" +
                  appended + "|" + sorted + "|" + symbolEntry[0] + "=" + symbolEntry[1];
                "#,
            )
            .expect("URLSearchParams iteration should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some(
                "mode|mode|one|two|mode|one|mode=one;mode=two;tag=rust;|3|?mode=one&mode=two&tag=rust&mode=three|?mode=one&mode=two&mode=three&tag=rust|mode=one",
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
    fn form_submission_rejects_unsupported_enctype() {
        let document = document(
            r#"<form id="profile" method="post" enctype="application/json"><input name="name" value="Ceri"></form>"#,
        );
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document, ResourceBundle::default(), base);
        let error = runtime
            .execute("document.getElementById(\"profile\").requestSubmit();")
            .expect_err("unsupported form encodings should fail explicitly");
        assert_eq!(error.message, "unsupported form enctype application/json");
    }

    #[test]
    fn form_data_includes_an_empty_file_entry_without_a_selection() {
        let document = document(
            r#"<form id="profile"><input id="file" name="upload" type="file"></form><p id="status">idle</p>"#,
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const file = new FormData(document.getElementById("profile")).get("upload");
                document.getElementById("status").textContent = file.name + "|" + file.type + "|" + file.size;
                "#,
            )
            .expect("empty file entry should be available");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("|application/octet-stream|0")
        );

        let error = runtime
            .execute("document.getElementById(\"file\").value = \"chosen.txt\";")
            .expect_err("file input values cannot be assigned a path");
        assert_eq!(error.message, "property value is not writable");
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
    fn label_activation_focuses_and_clicks_associated_controls() {
        let document = document(
            r#"<label id="explicit" for="name">Name</label><input id="name"><label id="wrapped"><span id="caption">Remember</span><input id="remember" type="checkbox"></label><p id="status">idle</p>"#,
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const name = document.getElementById("name");
                const remember = document.getElementById("remember");
                const status = document.getElementById("status");
                name.addEventListener("focus", () => { status.textContent = "focused:" + document.activeElement.id; });
                remember.addEventListener("change", () => { status.textContent = "checked:" + remember.checked; });
                "#,
            )
            .expect("label listeners should register");
        assert!(runtime
            .click_id("explicit")
            .expect("explicit label should activate its control"));
        assert_eq!(runtime.active_element(), Some(vec![1]));
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("focused:name")
        );
        assert!(runtime
            .click_id("caption")
            .expect("wrapped label should activate its control"));
        assert_eq!(runtime.active_element(), Some(vec![2, 1]));
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("checked:true")
        );
    }

    #[test]
    fn prevented_label_activation_does_not_trigger_the_control() {
        let document = document(
            r#"<label id="guarded" for="check">Check</label><input id="check" type="checkbox"><p id="status">idle</p>"#,
        );
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                document.getElementById("guarded").addEventListener("click", (event) => event.preventDefault());
                "#,
            )
            .expect("label listener should register");
        assert!(runtime
            .click_id("guarded")
            .expect("prevented label click should be handled"));
        assert_eq!(runtime.active_element(), None);
        assert_eq!(
            runtime
                .execute("document.getElementById(\"status\").textContent = document.getElementById(\"check\").checked;")
                .map(|_| ()),
            Ok(())
        );
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(document.borrow().element_text_content(&status).as_deref(), Some("false"));
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
    fn response_headers_support_membership_iteration_and_for_each() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            Some(Rc::new(HeadersFetchProvider {
                requests: requests.clone(),
            })),
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                fetch("/headers").then((response) => {
                    const keys = response.headers.keys();
                    const first = response.headers.entries().next();
                    let visited = "";
                    response.headers.forEach((value, name) => {
                        visited = visited + name + "=" + value + ";";
                    });
                    status.textContent = response.headers.has("X-Alpha") + "|" + response.headers.has("missing") + "|" + keys.join(",") + "|" + first.value[0] + "=" + first.value[1] + "|" + visited;
                });
                "#,
            )
            .expect("response header iteration should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some(
                "true|false|content-type,x-alpha,x-zed|content-type=text/plain|content-type=text/plain;x-alpha=first;x-zed=last;"
            )
        );
        assert_eq!(requests.borrow().len(), 1);
    }

    #[test]
    fn headers_construct_mutate_requests_and_keep_responses_immutable() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            Some(Rc::new(HeadersFetchProvider {
                requests: requests.clone(),
            })),
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const requestHeaders = new Headers([["X-Test", "one"], ["X-Test", "two"]]);
                requestHeaders.append("X-Test", "three");
                requestHeaders.set("X-Remove", "value");
                requestHeaders.delete("X-Remove");
                const objectHeaders = new Headers({ "X-Object": "value" });
                requestHeaders.set("X-Object", objectHeaders.get("x-object"));
                const copiedHeaders = new Headers(requestHeaders);
                copiedHeaders.set("X-Copy", "yes");
                fetch("/headers", { headers: copiedHeaders }).then((response) => {
                    let immutable = "";
                    try { response.headers.set("x-new", "changed"); } catch (error) { immutable = error.message; }
                    status.textContent = response.headers.get("x-new") + "|" + immutable;
                });
                "#,
            )
            .expect("Headers constructor and mutation should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("null|headers are immutable")
        );
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].headers().get("x-test").map(String::as_str),
            Some("one, two, three")
        );
        assert_eq!(requests[0].headers().get("x-remove"), None);
        assert_eq!(
            requests[0].headers().get("x-copy").map(String::as_str),
            Some("yes")
        );
        assert_eq!(
            requests[0].headers().get("x-object").map(String::as_str),
            Some("value")
        );
    }

    #[test]
    fn request_objects_clone_inspect_and_dispatch_their_body() {
        let document = document("<p id='status'>idle</p><p id='body'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            Some(Rc::new(HeadersFetchProvider {
                requests: requests.clone(),
            })),
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const base = new Request("/headers", {
                    method: "POST",
                    headers: { "X-Base": "base" },
                    body: "base body",
                    cache: "reload",
                    redirect: "manual"
                });
                const copied = new Request(base, {
                    method: "PUT",
                    headers: new Headers({ "X-Override": "yes" }),
                    body: "override body",
                    cache: "force-cache",
                    redirect: "follow"
                });
                const bodyCopy = copied.clone();
                bodyCopy.text().then((body) => {
                    document.getElementById("body").textContent = body + "|" + bodyCopy.bodyUsed;
                });
                const dispatched = copied.clone();
                fetch(dispatched).then((response) => {
                    let cloneError = "";
                    try { dispatched.clone(); } catch (error) { cloneError = error.message; }
                    status.textContent = base.method + "|" + base.url + "|" + base.cache + "|" + base.redirect + "|" + copied.method + "|" + copied.headers.get("x-override") + "|" + (copied.body === copied.body) + "|" + base.bodyUsed + "|" + copied.bodyUsed + "|" + dispatched.bodyUsed + "|" + cloneError;
                });
                "#,
            )
            .expect("Request construction and dispatch should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some(
                "POST|https://app.ceri.invalid/headers|reload|manual|PUT|yes|true|false|false|true|request body is already used"
            )
        );
        let body = document
            .borrow()
            .find_element_by_id("body")
            .expect("body status should exist");
        assert_eq!(
            document.borrow().element_text_content(&body).as_deref(),
            Some("override body|true")
        );
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method(), "PUT");
        assert_eq!(requests[0].body(), b"override body");
        assert_eq!(
            requests[0].headers().get("x-override").map(String::as_str),
            Some("yes")
        );
        assert_eq!(requests[0].headers().get("x-base"), None);
        assert_eq!(requests[0].cache_mode(), &ResourceCacheMode::ForceCache);
    }

    #[test]
    fn response_constructor_exposes_status_body_clone_and_json() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            None,
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const response = new Response("created", {
                    status: 201,
                    statusText: "Created",
                    headers: { "X-Trace": "one" }
                });
                response.headers.set("X-Added", "yes");
                const clone = response.clone();
                clone.headers.set("X-Clone", "yes");
                clone.text().then((body) => {
                    const json = Response.json({ answer: 42 }, { status: 202, statusText: "Accepted" });
                    json.text().then((jsonBody) => {
                        let cloneError = "";
                        try { clone.clone(); } catch (error) { cloneError = error.message; }
                        let invalidStatus = "";
                        try { new Response("body", { status: 600 }); } catch (error) { invalidStatus = error.message; }
                        let nullBody = "";
                        try { new Response("", { status: 204 }); } catch (error) { nullBody = error.message; }
                        status.textContent = response.type + "|" + response.url + "|" + response.status + "|" + response.statusText + "|" + response.ok + "|" + response.headers.get("x-added") + "|" + response.headers.get("x-clone") + "|" + response.bodyUsed + "|" + clone.bodyUsed + "|" + body + "|" + cloneError + "|" + json.status + "|" + json.statusText + "|" + json.mimeType + "|" + json.headers.get("content-type") + "|" + jsonBody + "|" + invalidStatus + "|" + nullBody;
                    });
                });
                "#,
            )
            .expect("Response construction should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some(
                "default||201|Created|true|yes|null|false|true|created|response body is already used|202|Accepted|application/json|application/json|{\"answer\":42}|Response status must be an integer between 200 and 599|Response body is not allowed for this status"
            )
        );
    }

    #[test]
    fn response_static_factories_build_error_and_redirects() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            None,
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const errorResponse = Response.error();
                const redirectResponse = Response.redirect("/next", 307);
                let immutable = "";
                try { redirectResponse.headers.set("x-test", "blocked"); } catch (error) { immutable = error.message; }
                let invalidStatus = "";
                try { Response.redirect("/next", 200); } catch (error) { invalidStatus = error.message; }
                status.textContent = errorResponse.type + "|" + errorResponse.status + "|" + errorResponse.ok + "|" + errorResponse.url + "|" + redirectResponse.type + "|" + redirectResponse.status + "|" + redirectResponse.url + "|" + redirectResponse.headers.get("location") + "|" + redirectResponse.bodyUsed + "|" + immutable + "|" + invalidStatus;
                "#,
            )
            .expect("Response static factories should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some(
                "error|0|false||default|307||https://app.ceri.invalid/next|false|headers are immutable|Response redirect status is invalid"
            )
        );
    }

    #[test]
    fn url_search_params_bodies_encode_and_infer_content_type() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            Some(Rc::new(HeadersFetchProvider {
                requests: requests.clone(),
            })),
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const params = new URLSearchParams([["q", "hello world"], ["tag", "rust"]]);
                const request = new Request("/headers", { method: "POST", body: params });
                fetch(request).then((response) => response.text()).then((body) => {
                    fetch("/headers", {
                        method: "POST",
                        headers: { "Content-Type": "application/custom" },
                        body: new URLSearchParams([["custom", "yes"]])
                    }).then(() => { status.textContent = body; });
                });
                "#,
            )
            .expect("URLSearchParams bodies should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("ready")
        );
        let requests = requests.borrow();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].body(), b"q=hello+world&tag=rust");
        assert_eq!(
            requests[0].headers().get("content-type").map(String::as_str),
            Some("application/x-www-form-urlencoded;charset=UTF-8")
        );
        assert_eq!(requests[1].body(), b"custom=yes");
        assert_eq!(
            requests[1].headers().get("content-type").map(String::as_str),
            Some("application/custom")
        );
    }

    #[test]
    fn form_data_reads_decode_urlencoded_body_and_consume_once() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            Some(Rc::new(HeadersFetchProvider {
                requests: requests.clone(),
            })),
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const request = new Request("/headers", { method: "POST", body: new URLSearchParams([["q", "hello world"], ["q", "second"]]) });
                request.formData().then((data) => {
                    data.append("extra", "yes");
                    fetch("/form").then((response) => response.formData()).then((responseData) => {
                        status.textContent = data.get("q") + "|" + data.getAll("q").length + "|" + data.get("extra") + "|" + responseData.get("answer") + "|" + responseData.getAll("answer").length;
                        status.textContent = status.textContent + "|" + request.bodyUsed;
                    }, (error) => { status.textContent = error.message; });
                });
                "#,
            )
            .expect("formData body reads should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("hello world|2|yes|42|2|true")
        );
        assert_eq!(requests.borrow().len(), 1);
    }

    #[test]
    fn blob_bodies_preserve_bytes_types_and_response_reads() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            Some(Rc::new(HeadersFetchProvider {
                requests: requests.clone(),
            })),
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const source = new Blob(["hello ", new Blob(["world"], { type: "text/inner" })], { type: "TEXT/PLAIN" });
                const part = source.slice(-5, 11, "Application/Test");
                source.arrayBuffer().then((buffer) => {
                    part.text().then((partText) => {
                        fetch("/headers", { method: "POST", body: source }).then((response) => response.blob()).then((received) => {
                            received.text().then((body) => {
                                status.textContent = source.size + "|" + buffer.byteLength + "|" + source.type + "|" + part.size + "|" + part.type + "|" + partText + "|" + body + "|" + received.type + "|" + received.size + "|" + (source instanceof Blob);
                            });
                        });
                    });
                });
                "#,
            )
            .expect("Blob bodies should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("11|11|text/plain|5|application/test|world|ready|text/plain|5|true")
        );
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].body(), b"hello world");
        assert_eq!(
            requests[0].headers().get("content-type").map(String::as_str),
            Some("text/plain")
        );
    }

    #[test]
    fn file_values_preserve_metadata_and_use_blob_body_rules() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            Some(Rc::new(HeadersFetchProvider {
                requests: requests.clone(),
            })),
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const file = new File(["report"], "docs\\report.txt", { type: "TEXT/PLAIN", lastModified: 1234.9 });
                const part = file.slice(-4, 6, "Application/Part");
                file.text().then((fileText) => file.arrayBuffer().then((buffer) => part.text().then((partText) => {
                    fetch("/headers", { method: "POST", body: file }).then((response) => response.blob()).then((received) => received.text().then((body) => {
                        status.textContent = file.name + "|" + file.lastModified + "|" + file.size + "|" + file.type + "|" + fileText + "|" + buffer.byteLength + "|" + part.size + "|" + part.type + "|" + partText + "|" + body + "|" + (file instanceof Blob) + "|" + (file instanceof File);
                    }));
                })));
                "#,
            )
            .expect("File bodies should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("report.txt|1234|6|text/plain|report|6|4|application/part|port|ready|true|true")
        );
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].body(), b"report");
        assert_eq!(
            requests[0].headers().get("content-type").map(String::as_str),
            Some("text/plain")
        );
    }

    #[test]
    fn form_data_bodies_encode_ordered_text_entries() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            Some(Rc::new(HeadersFetchProvider {
                requests: requests.clone(),
            })),
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const data = new FormData();
                data.append("field", "first");
                data.append("note", "line\nvalue");
                fetch("/headers", { method: "POST", body: data }).then(() => { status.textContent = "done"; });
                "#,
            )
            .expect("FormData bodies should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("done")
        );
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].headers().get("content-type").map(String::as_str),
            Some("multipart/form-data; boundary=----ceri-form-data-boundary")
        );
        assert_eq!(
            requests[0].body(),
            b"------ceri-form-data-boundary\r\nContent-Disposition: form-data; name=\"field\"\r\n\r\nfirst\r\n------ceri-form-data-boundary\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\nline\nvalue\r\n------ceri-form-data-boundary--\r\n"
        );
    }

    #[test]
    fn form_data_multipart_escapes_parameters_and_avoids_boundary_collisions() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            Some(Rc::new(HeadersFetchProvider {
                requests: requests.clone(),
            })),
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const data = new FormData();
                data.append("na\"me\\\r\n", "prefix----ceri-form-data-boundary");
                fetch("/headers", { method: "POST", body: data }).then(() => { status.textContent = "done"; });
                "#,
            )
            .expect("FormData multipart escaping should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("done")
        );
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].headers().get("content-type").map(String::as_str),
            Some("multipart/form-data; boundary=----ceri-form-data-boundaryx")
        );
        let body = String::from_utf8(requests[0].body().to_vec())
            .expect("text-only FormData should use UTF-8 bytes");
        assert!(body.starts_with(
            "------ceri-form-data-boundaryx\r\nContent-Disposition: form-data; name=\"na\\\"me\\\\%0D%0A\"\r\n\r\n"
        ));
        assert!(body.contains("prefix----ceri-form-data-boundary\r\n"));
        assert!(body.ends_with("------ceri-form-data-boundaryx--\r\n"));
    }

    #[test]
    fn form_data_file_entries_preserve_order_and_emit_file_parts() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            Some(Rc::new(HeadersFetchProvider {
                requests: requests.clone(),
            })),
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const data = new FormData();
                const blob = new Blob(["blob----ceri-form-data-boundary"], { type: "application/blob" });
                const file = new File(["file-bytes"], "dir/report.bin", { type: "application/file" });
                data.append("text", "one");
                data.append("blob", blob);
                data.append("file", file, "uploads/custom.bin");
                data.set("text", "two");
                const blobEntry = data.get("blob");
                const fileEntry = data.get("file");
                fetch("/headers", { method: "POST", body: data }).then(() => {
                    status.textContent = blobEntry.name + "|" + blobEntry.type + "|" + fileEntry.name + "|" + fileEntry.type + "|" + data.getAll("file").length;
                });
                "#,
            )
            .expect("FormData file entries should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("blob|application/blob|custom.bin|application/file|1")
        );
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].headers().get("content-type").map(String::as_str),
            Some("multipart/form-data; boundary=----ceri-form-data-boundaryx")
        );
        assert_eq!(
            requests[0].body(),
            b"------ceri-form-data-boundaryx\r\nContent-Disposition: form-data; name=\"text\"\r\n\r\ntwo\r\n------ceri-form-data-boundaryx\r\nContent-Disposition: form-data; name=\"blob\"; filename=\"blob\"\r\nContent-Type: application/blob\r\n\r\nblob----ceri-form-data-boundary\r\n------ceri-form-data-boundaryx\r\nContent-Disposition: form-data; name=\"file\"; filename=\"custom.bin\"\r\nContent-Type: application/file\r\n\r\nfile-bytes\r\n------ceri-form-data-boundaryx--\r\n"
        );
    }

    #[test]
    fn form_data_multipart_round_trip_restores_text_and_file_entries() {
        let document = document("<p id='status'>idle</p>");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            Some(Rc::new(HeadersFetchProvider {
                requests: requests.clone(),
            })),
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const data = new FormData();
                data.append("text", "two");
                data.append("text", "three");
                data.append("blob", new Blob(["blob-bytes"], { type: "application/blob" }));
                data.append("file", new File(["file-bytes"], "custom.bin", { type: "application/file" }));
                fetch("/echo-form", { method: "POST", body: data }).then((response) => response.formData()).then((roundTrip) => {
                    const file = roundTrip.get("file");
                    file.text().then((fileText) => {
                        const blob = roundTrip.get("blob");
                        status.textContent = roundTrip.get("text") + "|" + roundTrip.getAll("text").length + "|" + blob.name + "|" + blob.type + "|" + file.name + "|" + file.type + "|" + fileText;
                    });
                });
                "#,
            )
            .expect("multipart FormData round trip should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("two|2|blob|application/blob|custom.bin|application/file|file-bytes")
        );
        assert_eq!(requests.borrow().len(), 1);
    }

    #[test]
    fn form_data_iteration_preserves_order_and_file_values() {
        let document = document("<p id='status'>idle</p>");
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const data = new FormData();
                data.append("first", "one");
                data.append("file", new File(["bytes"], "name.txt", { type: "text/plain" }));
                data.append("first", "two");
                const keys = data.keys();
                const firstKey = keys.next().value;
                const secondKey = keys.next().value;
                const values = data.values();
                const firstValue = values.next().value;
                const fileValue = values.next().value;
                const firstEntry = data.entries().next().value;
                const symbolEntry = data[Symbol.iterator]().next().value;
                const spread = [...data];
                let seen = "";
                data.forEach((value, name) => { seen = seen + name + ";"; });
                document.getElementById("status").textContent =
                  firstKey + "|" + secondKey + "|" + firstValue + "|" + fileValue.name + "|" +
                  firstEntry[0] + "|" + firstEntry[1] + "|" + seen + "|" + spread.length + "|" +
                  symbolEntry[0] + "|" + spread[1][1].name;
                "#,
            )
            .expect("FormData iteration should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("first|file|one|name.txt|first|one|first;file;first;|3|first|name.txt")
        );
    }

    #[test]
    fn fetch_follows_same_origin_redirects_and_reports_final_response_state() {
        let document = document("<p id='status'>idle</p>");
        let requests = Rc::new(RefCell::new(Vec::new()));
        let provider = Rc::new(RedirectFetchProvider {
            requests: requests.clone(),
        });
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            Some(provider),
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                fetch("/redirect").then((response) => {
                    response.text().then((body) => {
                        document.getElementById("status").textContent = response.status + "|" + response.redirected + "|" + response.url + "|" + body;
                    });
                });
                "#,
            )
            .expect("fetch redirect should follow");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("200|true|https://app.ceri.invalid/final|followed")
        );
        assert_eq!(
            requests
                .borrow()
                .iter()
                .map(|request| request.url().path())
                .collect::<Vec<_>>(),
            vec!["/redirect", "/final"]
        );
    }

    #[test]
    fn fetch_manual_and_error_redirect_modes_are_observable() {
        let document = document("<p id='status'>idle</p>");
        let provider = Rc::new(RedirectFetchProvider {
            requests: Rc::new(RefCell::new(Vec::new())),
        });
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            Some(provider),
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                fetch("/redirect", { redirect: "manual" }).then((response) => {
                    document.getElementById("status").textContent = response.status + "|" + response.redirected + "|" + response.url;
                    fetch("/redirect", { redirect: "error" }).catch((error) => {
                        document.getElementById("status").textContent = document.getElementById("status").textContent + "|" + error;
                    });
                });
                "#,
            )
            .expect("fetch redirect modes should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("302|false|https://app.ceri.invalid/redirect|fetch redirect was rejected")
        );
    }

    #[test]
    fn fetch_303_rewrites_post_to_get_without_a_body() {
        let document = document("<p id='status'>idle</p>");
        let requests = Rc::new(RefCell::new(Vec::new()));
        let provider = Rc::new(RedirectFetchProvider {
            requests: requests.clone(),
        });
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            Some(ResourceBundle::default()),
            Some(provider),
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                fetch("/submit", { method: "post", body: "payload" }).then((response) => {
                    response.text().then((body) => { document.getElementById("status").textContent = body; });
                });
                "#,
            )
            .expect("POST redirect should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("submitted")
        );
        let requests = requests.borrow();
        assert_eq!(requests[0].method(), "POST");
        assert_eq!(requests[0].body(), b"payload");
        assert_eq!(requests[1].method(), "GET");
        assert!(requests[1].body().is_empty());
    }

    #[test]
    fn abort_controller_dispatches_abort_and_rejects_preflight_fetch() {
        let document = document("<p id='status'>idle</p>");
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
                const controller = new AbortController();
                const signal = controller.signal;
                signal.addEventListener("abort", (event) => {
                    status.textContent = event.type + "|" + signal.aborted + "|" + signal.reason.name + "|" + signal.reason.message;
                });
                controller.abort();
                try {
                    signal.throwIfAborted();
                } catch (error) {
                    status.textContent = status.textContent + "|" + error.name + "|" + error.message;
                }
                fetch("/data.txt", { signal: signal }).catch((error) => {
                    status.textContent = status.textContent + "|" + error.name + "|" + error.message;
                });
                "#,
            )
            .expect("aborted fetch should remain a rejected promise");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some(
                "abort|true|AbortError|The operation was aborted|AbortError|The operation was aborted|AbortError|The operation was aborted"
            )
        );
    }

    #[test]
    fn active_abort_signal_can_be_passed_to_fetch() {
        let document = document("<p id='status'>idle</p>");
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
                const controller = new AbortController();
                fetch("/data.txt", { signal: controller.signal })
                    .then((response) => response.text())
                    .then((body) => { status.textContent = body + "|" + controller.signal.aborted; });
                "#,
            )
            .expect("active signal fetch should resolve");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("ready|false")
        );
    }

    #[test]
    fn response_body_consumption_is_single_use() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert("/data.txt", "text/plain", b"hello".to_vec())
            .expect("resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                fetch("/data.txt").then((response) => {
                    response.text().then((body) => {
                        status.textContent = body + "|" + response.bodyUsed;
                        response.text().catch((error) => {
                            status.textContent = status.textContent + "|" + error;
                        });
                    });
                });
                "#,
            )
            .expect("response body should be consumed once");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("hello|true|response body is already used")
        );
    }

    #[test]
    fn response_body_stream_identity_is_stable_across_property_reads() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert("/data.txt", "text/plain", b"hello".to_vec())
            .expect("resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                fetch("/data.txt").then((response) => {
                    const first = response.body;
                    const second = response.body;
                    status.textContent = (first === second) + "|" + first.locked;
                    second.getReader().cancel().then(() => {
                        status.textContent = status.textContent + "|" + response.bodyUsed;
                    });
                });
                "#,
            )
            .expect("response body identity should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("true|false|true")
        );
    }

    #[test]
    fn response_clone_preserves_metadata_and_has_an_independent_body() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert("/data.txt", "text/plain", b"hello".to_vec())
            .expect("resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                fetch("/data.txt").then((response) => {
                    const copy = response.clone();
                    response.text().then((original) => {
                        copy.text().then((cloned) => {
                            status.textContent = original + "|" + cloned + "|" + copy.status + "|" + copy.url;
                        });
                    });
                });
                "#,
            )
            .expect("response clone should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("hello|hello|200|https://app.ceri.invalid/data.txt")
        );
    }

    #[test]
    fn response_clone_rejects_a_disturbed_body() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert("/data.txt", "text/plain", b"hello".to_vec())
            .expect("resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                fetch("/data.txt").then((response) => {
                    response.text().then(() => {
                        try { response.clone(); } catch (error) { status.textContent = error; }
                    });
                });
                "#,
            )
            .expect("response clone rejection should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("response body is already used")
        );
    }

    #[test]
    fn response_body_stream_reads_fixed_chunks_and_reports_done() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/data.txt",
                "text/plain",
                b"0123456789abcdefghijklmnop".to_vec(),
            )
            .expect("resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                fetch("/data.txt").then((response) => {
                    const stream = response.body;
                    const reader = stream.getReader();
                    reader.read().then((first) => {
                        reader.read().then((second) => {
                            reader.read().then((third) => {
                                status.textContent = stream.locked + "|" + response.bodyUsed + "|" + first.value.length + "|" + second.value.length + "|" + third.done;
                                reader.releaseLock();
                            });
                        });
                    });
                });
                "#,
            )
            .expect("response body stream should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("true|true|16|10|true")
        );
    }

    #[test]
    fn response_body_stream_tee_clones_remaining_bytes_and_locks_source() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert("/data.txt", "text/plain", b"abcd".to_vec())
            .expect("resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                fetch("/data.txt").then((response) => {
                    const pair = response.body.tee();
                    const left = pair[0].getReader();
                    const right = pair[1].getReader();
                    left.read().then((first) => {
                        right.read().then((second) => {
                            left.read().then((end) => {
                                status.textContent = response.bodyUsed + "|" + first.value.length + "|" + second.value.length + "|" + end.done;
                            });
                        });
                    });
                });
                "#,
            )
            .expect("response body tee should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("true|4|4|true")
        );
    }

    #[test]
    fn response_array_buffer_exposes_bytes_and_body_used_state() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert("/data.bin", "application/octet-stream", vec![4, 8, 15, 16])
            .expect("resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                fetch("/data.bin").then((response) => {
                    response.arrayBuffer().then((buffer) => {
                        status.textContent = buffer.byteLength + "|" + buffer[1] + "|" + response.bodyUsed;
                    });
                });
                "#,
            )
            .expect("arrayBuffer should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("4|8|true")
        );
    }

    #[test]
    fn response_reader_cancel_disturbs_the_shared_body() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert("/data.txt", "text/plain", b"cancel me".to_vec())
            .expect("resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                fetch("/data.txt").then((response) => {
                    const reader = response.body.getReader();
                    reader.cancel("stop").then(() => {
                        response.text().catch((error) => {
                            status.textContent = response.bodyUsed + "|" + error;
                        });
                    });
                });
                "#,
            )
            .expect("reader cancellation should execute");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("true|response body is already used")
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
    fn worker_dispatches_structured_messages_without_a_loopback_server() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/worker.js",
                "text/javascript",
                br#"self.onmessage = (event) => {
                  postMessage({ kind: event.data.kind, text: event.data.text + "!" });
                };"#
                .to_vec(),
            )
            .expect("worker resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const worker = new Worker("/worker.js", { name: "echo" });
                worker.onmessage = (event) => {
                  status.textContent = worker.name + "|" + event.data.kind + "|" + event.data.text;
                };
                worker.postMessage({ kind: "greeting", text: "hello" });
                "#,
            )
            .expect("worker construction and message send should execute");
        for _ in 0..100 {
            if runtime.pump().expect("worker pump should execute") > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("echo|greeting|hello!")
        );
    }

    #[test]
    fn worker_loads_its_entry_script_through_the_resource_provider() {
        let document = document("<p id='status'>idle</p>");
        let requests = Rc::new(RefCell::new(Vec::new()));
        let provider = Rc::new(WorkerScriptProvider {
            requests: requests.clone(),
        });
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_context(
            document.clone(),
            None,
            Some(provider),
            Some(base),
            None,
            None,
            None,
        );
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const worker = new Worker("/worker.js");
                worker.onmessage = (event) => { status.textContent = event.data; };
                worker.postMessage("remote");
                "#,
            )
            .expect("provider-backed worker construction should execute");
        for _ in 0..100 {
            if runtime.pump().expect("worker pump should execute") > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("remote!")
        );
        assert_eq!(requests.borrow().as_slice(), ["/worker.js"]);
    }

    #[test]
    fn worker_rejects_unknown_script_types_and_unavailable_resources() {
        let document = document("<p>worker</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert("/worker.js", "text/javascript", b"postMessage(1);".to_vec())
            .expect("worker resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        let type_error = runtime
            .execute(r#"new Worker("/worker.js", { type: "shared" });"#)
            .expect_err("unknown worker types should be rejected");
        assert!(type_error
            .message
            .contains("unsupported Worker script type shared"));
        let missing_error = runtime
            .execute(r#"new Worker("/missing.js");"#)
            .expect_err("missing packaged workers should be rejected");
        assert!(missing_error.message.contains("worker script /missing.js was not found"));
    }

    #[test]
    fn module_worker_loads_packaged_same_origin_imports() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/worker-module.js",
                "text/javascript",
                br#"import { suffix } from "./worker-module-child.js";
                self.addEventListener("message", (event) => { postMessage(suffix(event.data)); });"#
                    .to_vec(),
            )
            .expect("module worker resource should be accepted");
        resources
            .insert(
                "/worker-module-child.js",
                "text/javascript",
                b"export function suffix(value) { return value + '!'; }".to_vec(),
            )
            .expect("module worker child should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const worker = new Worker("/worker-module.js", { type: "module" });
                worker.addEventListener("message", (event) => { status.textContent = event.data; });
                worker.postMessage("module");
                "#,
            )
            .expect("module worker construction should execute");
        for _ in 0..100 {
            if runtime.pump().expect("worker pump should execute") > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("module!")
        );
    }

    #[test]
    fn worker_reports_startup_errors_to_onerror() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/broken-worker.js",
                "text/javascript",
                b"missingWorkerGlobal;".to_vec(),
            )
            .expect("worker resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const worker = new Worker("/broken-worker.js");
                worker.onerror = (event) => { status.textContent = event.message; };
                "#,
            )
            .expect("worker construction should queue startup errors");
        for _ in 0..100 {
            if runtime.pump().expect("worker pump should execute") > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("unknown identifier missingWorkerGlobal")
        );
    }

    #[test]
    fn worker_terminate_closes_the_page_message_endpoint() {
        let document = document("<p id='status'>idle</p>");
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/idle-worker.js",
                "text/javascript",
                b"self.onmessage = (event) => { postMessage(event.data); };".to_vec(),
            )
            .expect("worker resource should be accepted");
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources(document.clone(), resources, base);
        runtime
            .execute(
                r#"
                const status = document.getElementById("status");
                const worker = new Worker("/idle-worker.js");
                worker.terminate();
                try {
                  worker.postMessage("ignored");
                } catch (error) {
                  status.textContent = error;
                }
                "#,
            )
            .expect("terminated worker should report a synchronous send error");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("Worker has been terminated")
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
