use crate::css::{
    AnimatableProperty, ColorScheme, StyleContext, StyleState, Stylesheet, TransitionState,
};
use crate::dom::{AccessibilityNode, Document, NodeId, ScriptSource, StylesheetSource};
use crate::font::FontSet;
use crate::image::Image;
use crate::layout::{
    ImageLayer, LayoutDocument, RangeDrag, Size, ThumbDrag, layout_with_state as build_layout,
};
use crate::paint::Canvas;
use crate::script::{
    HostRuntimeInfo, KeyModifiers, LayoutInputs, ModuleSourceLoader, NavigationRequest,
    ScriptRuntime, StorageData, StorageHandle, ViewportMetrics, runtime_error,
};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

// the milliseconds between frames while something animates
const FRAME_INTERVAL: u64 = 16;

pub const APP_ORIGIN: &str = "https://app.ceri.invalid";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppUrl {
    path: String,
    query: String,
    fragment: String,
}

impl AppUrl {
    pub fn parse(input: &str) -> Result<Self, NavigationError> {
        let scheme_end = input
            .find("://")
            .ok_or_else(|| NavigationError::InvalidUrl(input.to_string()))?;
        if !input[..scheme_end].eq_ignore_ascii_case("https") {
            return Err(NavigationError::InsecureScheme);
        }

        let remainder = &input[scheme_end + 3..];
        let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
        let authority = &remainder[..authority_end];
        if authority.contains('@') {
            return Err(NavigationError::Credentials);
        }
        if authority.contains(':') {
            return Err(NavigationError::Port);
        }
        if !authority.eq_ignore_ascii_case("app.ceri.invalid") {
            return Err(NavigationError::WrongOrigin);
        }

        let target = &remainder[authority_end..];
        let (target, fragment) = target
            .split_once('#')
            .map_or((target, ""), |(target, fragment)| (target, fragment));
        let (raw_path, query) = target
            .split_once('?')
            .map_or((target, ""), |(path, query)| (path, query));
        let raw_path = if raw_path.is_empty() { "/" } else { raw_path };

        Ok(Self {
            path: decode_path(raw_path)?,
            query: query.to_string(),
            fragment: fragment.to_string(),
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn fragment(&self) -> &str {
        &self.fragment
    }

    pub fn with_query_fragment(
        &self,
        query: impl Into<String>,
        fragment: impl Into<String>,
    ) -> Self {
        Self {
            path: self.path.clone(),
            query: query.into(),
            fragment: fragment.into(),
        }
    }

    pub fn with_path(&self, path: &str) -> Result<Self, NavigationError> {
        let path = if path.is_empty() { "/" } else { path };
        Self::parse(&format!("{APP_ORIGIN}{path}"))
    }

    pub fn resolve(&self, reference: &str) -> Result<Self, NavigationError> {
        if reference.contains("://") {
            return Self::parse(reference);
        }
        let (target, fragment) = reference
            .split_once('#')
            .map_or((reference, ""), |(target, fragment)| (target, fragment));
        let (raw_path, query, query_present) = target
            .split_once('?')
            .map_or((target, "", false), |(path, query)| (path, query, true));
        let path = if raw_path.is_empty() {
            self.path.clone()
        } else if raw_path.starts_with('/') {
            raw_path.to_string()
        } else {
            let directory = if self.path.ends_with('/') {
                self.path.as_str()
            } else {
                self.path.rsplit_once('/').map_or("/", |(directory, _)| {
                    if directory.is_empty() { "/" } else { directory }
                })
            };
            if directory == "/" {
                format!("/{raw_path}")
            } else {
                format!("{directory}/{raw_path}")
            }
        };
        Ok(Self {
            path: resolve_reference_path(&path)?,
            query: if query_present {
                query.to_string()
            } else if raw_path.is_empty() {
                self.query.clone()
            } else {
                String::new()
            },
            fragment: fragment.to_string(),
        })
    }
}

impl fmt::Display for AppUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{APP_ORIGIN}{}", self.path)?;
        if !self.query.is_empty() {
            write!(formatter, "?{}", self.query)?;
        }
        if !self.fragment.is_empty() {
            write!(formatter, "#{}", self.fragment)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    mime_type: String,
    body: Vec<u8>,
}

impl Resource {
    pub fn new(mime_type: impl Into<String>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            mime_type: mime_type.into(),
            body: body.into(),
        }
    }

    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceResponse {
    status: u16,
    mime_type: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl ResourceResponse {
    pub fn new(
        status: u16,
        mime_type: impl Into<String>,
        headers: BTreeMap<String, String>,
        body: impl Into<Vec<u8>>,
    ) -> Result<Self, NavigationError> {
        if !(100..=599).contains(&status) {
            return Err(NavigationError::InvalidResponseStatus(status));
        }
        let mime_type = mime_type.into().trim().to_string();
        if mime_type.is_empty() {
            return Err(NavigationError::InvalidResponseMimeType);
        }
        Ok(Self {
            status,
            mime_type,
            headers: normalize_headers(headers)?,
            body: body.into(),
        })
    }

    pub(crate) fn from_resource(resource: Resource) -> Self {
        Self {
            status: 200,
            mime_type: resource.mime_type,
            headers: BTreeMap::new(),
            body: resource.body,
        }
    }

    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    pub fn headers(&self) -> &BTreeMap<String, String> {
        &self.headers
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceCacheMode {
    Default,
    NoStore,
    Reload,
    NoCache,
    ForceCache,
    OnlyIfCached,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceRequest {
    url: AppUrl,
    method: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    cache_mode: ResourceCacheMode,
}

impl ResourceRequest {
    pub fn new(
        url: AppUrl,
        method: impl Into<String>,
        headers: BTreeMap<String, String>,
        body: impl Into<Vec<u8>>,
    ) -> Result<Self, NavigationError> {
        let method = method.into().trim().to_ascii_uppercase();
        if method.is_empty() || !method.bytes().all(is_http_token_byte) {
            return Err(NavigationError::InvalidRequestMethod(method));
        }
        let normalized_headers = normalize_headers(headers)?;
        let body = body.into();
        if matches!(method.as_str(), "GET" | "HEAD") && !body.is_empty() {
            return Err(NavigationError::RequestBodyNotAllowed(method));
        }
        Ok(Self {
            url,
            method,
            headers: normalized_headers,
            body,
            cache_mode: ResourceCacheMode::Default,
        })
    }

    pub fn get(url: AppUrl) -> Self {
        Self {
            url,
            method: "GET".to_string(),
            headers: BTreeMap::new(),
            body: Vec::new(),
            cache_mode: ResourceCacheMode::Default,
        }
    }

    pub fn with_cache_mode(mut self, cache_mode: ResourceCacheMode) -> Self {
        self.cache_mode = cache_mode;
        self
    }

    pub fn url(&self) -> &AppUrl {
        &self.url
    }

    pub fn method(&self) -> &str {
        &self.method
    }

    pub fn headers(&self) -> &BTreeMap<String, String> {
        &self.headers
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }

    pub fn cache_mode(&self) -> &ResourceCacheMode {
        &self.cache_mode
    }
}

// a fetch outside the app origin, which the host allows or refuses on its own policy
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalRequest {
    url: String,
    method: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl ExternalRequest {
    pub fn new(
        url: impl Into<String>,
        method: impl Into<String>,
        headers: BTreeMap<String, String>,
        body: impl Into<Vec<u8>>,
    ) -> Result<Self, NavigationError> {
        let url = external_url(&url.into())?;
        let method = method.into().trim().to_ascii_uppercase();
        if method.is_empty() || !method.bytes().all(is_http_token_byte) {
            return Err(NavigationError::InvalidRequestMethod(method));
        }
        let body = body.into();
        if matches!(method.as_str(), "GET" | "HEAD") && !body.is_empty() {
            return Err(NavigationError::RequestBodyNotAllowed(method));
        }
        Ok(Self {
            url,
            method,
            headers: normalize_headers(headers)?,
            body,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn origin(&self) -> &str {
        let after_scheme = &self.url["https://".len()..];
        let authority = after_scheme
            .split_once('/')
            .map_or(after_scheme, |(authority, _)| authority);
        &self.url[..("https://".len() + authority.len())]
    }

    pub fn method(&self) -> &str {
        &self.method
    }

    pub fn headers(&self) -> &BTreeMap<String, String> {
        &self.headers
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

// only absolute https targets without credentials or fragments can leave the app origin
pub fn external_url(reference: &str) -> Result<String, NavigationError> {
    let reference = reference.trim();
    let Some(rest) = reference.strip_prefix("https://") else {
        if reference.starts_with("http://") {
            return Err(NavigationError::InsecureScheme);
        }
        return Err(NavigationError::InvalidUrl(reference.to_string()));
    };
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    if authority.is_empty()
        || authority.contains('@')
        || reference.contains('#')
        || reference.contains('\0')
        || reference.chars().any(char::is_whitespace)
    {
        return Err(NavigationError::InvalidUrl(reference.to_string()));
    }
    if authority.eq_ignore_ascii_case(APP_ORIGIN.trim_start_matches("https://")) {
        return Err(NavigationError::WrongOrigin);
    }
    Ok(reference.to_string())
}

pub trait ResourceProvider {
    fn provide(&self, url: &AppUrl) -> Result<Option<Resource>, NavigationError>;

    // the default refuses, so an embedder opts into outside network access explicitly
    fn provide_external(
        &self,
        _request: &ExternalRequest,
    ) -> Result<Option<ResourceResponse>, NavigationError> {
        Ok(None)
    }

    fn provide_request(
        &self,
        request: &ResourceRequest,
    ) -> Result<Option<Resource>, NavigationError> {
        self.provide(request.url())
    }

    fn provide_response(
        &self,
        request: &ResourceRequest,
    ) -> Result<Option<ResourceResponse>, NavigationError> {
        self.provide_request(request)
            .map(|resource| resource.map(ResourceResponse::from_resource))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePickerRequest {
    name: String,
    multiple: bool,
    accept: Option<String>,
}

impl FilePickerRequest {
    pub fn new(name: impl Into<String>, multiple: bool, accept: Option<String>) -> Self {
        Self {
            name: name.into(),
            multiple,
            accept,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn multiple(&self) -> bool {
        self.multiple
    }

    pub fn accept(&self) -> Option<&str> {
        self.accept.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSelection {
    name: String,
    mime_type: String,
    bytes: Vec<u8>,
    last_modified: i64,
}

impl FileSelection {
    pub fn new(
        name: impl Into<String>,
        mime_type: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
        last_modified: i64,
    ) -> Result<Self, NavigationError> {
        let name = name.into();
        if name.is_empty()
            || matches!(name.as_str(), "." | "..")
            || name
                .chars()
                .any(|character| character.is_control() || matches!(character, '/' | '\\'))
        {
            return Err(NavigationError::InvalidFileName(name));
        }
        if last_modified < 0 {
            return Err(NavigationError::InvalidFileMetadata);
        }
        let mime_type = mime_type.into();
        if !mime_type.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
            return Err(NavigationError::InvalidFileMimeType);
        }
        Ok(Self {
            name,
            mime_type: mime_type.to_ascii_lowercase(),
            bytes: bytes.into(),
            last_modified,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn last_modified(&self) -> i64 {
        self.last_modified
    }
}

pub trait FilePicker {
    fn pick_files(
        &self,
        request: &FilePickerRequest,
    ) -> Result<Option<Vec<FileSelection>>, NavigationError>;
}

pub trait ClipboardProvider {
    fn read_text(&self) -> Result<String, NavigationError>;

    fn write_text(&self, text: &str) -> Result<(), NavigationError>;
}

#[derive(Clone, Debug, Default)]
pub struct ResourceBundle {
    resources: BTreeMap<String, Resource>,
}

impl ResourceBundle {
    pub fn from_directory(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = fs::canonicalize(root)?;
        if !fs::metadata(&root)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "resource root is not a directory",
            ));
        }
        let mut resources = Self::default();
        let mut visited = BTreeSet::new();
        let mut budget = BundleBudget::default();
        collect_directory(&root, &root, &mut resources, &mut visited, &mut budget)?;
        Ok(resources)
    }

    pub fn insert(
        &mut self,
        path: &str,
        mime_type: &str,
        body: impl Into<Vec<u8>>,
    ) -> Result<(), NavigationError> {
        if mime_type.trim().is_empty() {
            return Err(NavigationError::InvalidResourcePath(
                "resource MIME type is empty".to_string(),
            ));
        }
        let path = normalize_resource_path(path)?;
        self.resources
            .insert(path, Resource::new(mime_type.trim(), body));
        Ok(())
    }

    pub fn resource(&self, path: &str) -> Option<&Resource> {
        let path = normalize_resource_path(path).ok()?;
        self.resources.get(&path)
    }

    fn load(&self, url: &AppUrl) -> Result<Resource, NavigationError> {
        if let Some(resource) = self.resources.get(url.path()) {
            return Ok(resource.clone());
        }
        if is_extensionless_path(url.path())
            && let Some(resource) = self.resources.get("/index.html")
        {
            return Ok(resource.clone());
        }
        Err(NavigationError::ResourceNotFound(url.path().to_string()))
    }

    fn load_subresource(&self, url: &AppUrl) -> Result<Resource, NavigationError> {
        self.resources
            .get(url.path())
            .cloned()
            .ok_or_else(|| NavigationError::ResourceNotFound(url.path().to_string()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationStatus {
    Loaded,
    ErrorPage { status: u16 },
}

impl NavigationStatus {
    pub fn is_error(self) -> bool {
        matches!(self, Self::ErrorPage { .. })
    }

    pub fn status_code(self) -> u16 {
        match self {
            Self::Loaded => 200,
            Self::ErrorPage { status } => status,
        }
    }
}

impl fmt::Display for NavigationStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loaded => formatter.write_str("loaded"),
            Self::ErrorPage { status } => write!(formatter, "app recovery page ({status})"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationFailure {
    pub url: AppUrl,
    pub status: u16,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavigationError {
    InvalidUrl(String),
    InsecureScheme,
    WrongOrigin,
    Credentials,
    Port,
    InvalidPercentEncoding,
    EncodedSeparator,
    Backslash,
    NulByte,
    Colon,
    UnsafeCharacter,
    UnsafeWindowsName,
    InvalidFileName(String),
    InvalidFileMimeType,
    InvalidFileMetadata,
    DotSegment,
    EmptySegment,
    InvalidResourcePath(String),
    InvalidRequestMethod(String),
    InvalidRequestHeader(String),
    RequestBodyNotAllowed(String),
    InvalidResponseStatus(u16),
    InvalidResponseMimeType,
    HttpStatus { path: String, status: u16 },
    ResourceNotFound(String),
    ResourceProvider(String),
    FilePicker(String),
    Clipboard(String),
    Storage(String),
    UnsupportedMimeType { path: String, mime_type: String },
    InvalidUtf8(String),
    Script(String),
    Stylesheet(String),
    NoPage,
    NavigationLoop,
}

impl fmt::Display for NavigationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(input) => write!(formatter, "invalid app URL: {input}"),
            Self::InsecureScheme => formatter.write_str("only the HTTPS app origin is supported"),
            Self::WrongOrigin => formatter.write_str("URL is outside the app origin"),
            Self::Credentials => formatter.write_str("URL credentials are not allowed"),
            Self::Port => formatter.write_str("URL ports are not allowed"),
            Self::InvalidPercentEncoding => formatter.write_str("URL has invalid percent encoding"),
            Self::EncodedSeparator => {
                formatter.write_str("encoded path separators are not allowed")
            }
            Self::Backslash => formatter.write_str("backslashes are not allowed in app paths"),
            Self::NulByte => formatter.write_str("NUL bytes are not allowed in app paths"),
            Self::Colon => formatter.write_str("colons are not allowed in app paths"),
            Self::UnsafeCharacter => {
                formatter.write_str("unsafe characters are not allowed in app paths")
            }
            Self::UnsafeWindowsName => {
                formatter.write_str("unsafe Windows path names are not allowed")
            }
            Self::InvalidFileName(name) => {
                write!(formatter, "invalid selected file name: {name}")
            }
            Self::InvalidFileMimeType => {
                formatter.write_str("selected file MIME type contains invalid bytes")
            }
            Self::InvalidFileMetadata => formatter.write_str("selected file metadata is invalid"),
            Self::DotSegment => formatter.write_str("dot segments are not allowed in app paths"),
            Self::EmptySegment => formatter.write_str("empty path segments are not allowed"),
            Self::InvalidResourcePath(path) => write!(formatter, "invalid resource path: {path}"),
            Self::InvalidRequestMethod(method) => {
                write!(formatter, "invalid resource request method: {method}")
            }
            Self::InvalidRequestHeader(header) => {
                write!(formatter, "invalid resource request header: {header}")
            }
            Self::RequestBodyNotAllowed(method) => {
                write!(
                    formatter,
                    "resource request body is not allowed for {method}"
                )
            }
            Self::InvalidResponseStatus(status) => {
                write!(formatter, "invalid resource response status: {status}")
            }
            Self::InvalidResponseMimeType => {
                formatter.write_str("resource response MIME type is empty")
            }
            Self::HttpStatus { path, status } => {
                write!(formatter, "resource request returned HTTP {status}: {path}")
            }
            Self::ResourceNotFound(path) => write!(formatter, "resource not found: {path}"),
            Self::ResourceProvider(detail) => {
                write!(formatter, "resource provider failed: {detail}")
            }
            Self::FilePicker(detail) => write!(formatter, "file picker failed: {detail}"),
            Self::Clipboard(detail) => write!(formatter, "clipboard failed: {detail}"),
            Self::Storage(detail) => write!(formatter, "storage failed: {detail}"),
            Self::UnsupportedMimeType { path, mime_type } => {
                write!(
                    formatter,
                    "unsupported resource type for {path}: {mime_type}"
                )
            }
            Self::InvalidUtf8(path) => write!(formatter, "resource is not UTF-8: {path}"),
            Self::Script(detail) => write!(formatter, "script execution failed: {detail}"),
            Self::Stylesheet(detail) => write!(formatter, "stylesheet parsing failed: {detail}"),
            Self::NoPage => formatter.write_str("the browser has no loaded page"),
            Self::NavigationLoop => formatter.write_str("navigation exceeded the redirect limit"),
        }
    }
}

impl std::error::Error for NavigationError {}

const MAX_NAVIGATION_REDIRECTS: usize = 16;

fn recovery_status(error: &NavigationError) -> u16 {
    match error {
        NavigationError::HttpStatus { status, .. } => *status,
        NavigationError::ResourceProvider(_)
        | NavigationError::FilePicker(_)
        | NavigationError::Clipboard(_)
        | NavigationError::Storage(_) => 503,
        _ => 500,
    }
}

fn load_provider_resource(
    provider: &dyn ResourceProvider,
    url: &AppUrl,
) -> Result<Option<(AppUrl, Resource)>, NavigationError> {
    load_provider_request_at(provider, &ResourceRequest::get(url.clone()), 0)
}

fn load_provider_request_at(
    provider: &dyn ResourceProvider,
    request: &ResourceRequest,
    redirect_depth: usize,
) -> Result<Option<(AppUrl, Resource)>, NavigationError> {
    let Some(response) = provider.provide_response(request)? else {
        return Ok(None);
    };
    if is_http_redirect(response.status()) {
        if redirect_depth >= MAX_NAVIGATION_REDIRECTS {
            return Err(NavigationError::NavigationLoop);
        }
        let Some(location) = response.headers().get("location") else {
            return Err(NavigationError::HttpStatus {
                path: request.url().path().to_string(),
                status: response.status(),
            });
        };
        let target = request.url().resolve(location)?;
        let request = redirect_navigation_request(request, target, response.status())?;
        return load_provider_request_at(provider, &request, redirect_depth + 1);
    }
    if !(200..300).contains(&response.status()) {
        return Err(NavigationError::HttpStatus {
            path: request.url().path().to_string(),
            status: response.status(),
        });
    }
    Ok(Some((
        request.url().clone(),
        Resource::new(response.mime_type().to_string(), response.body().to_vec()),
    )))
}

fn redirect_navigation_request(
    request: &ResourceRequest,
    target: AppUrl,
    status: u16,
) -> Result<ResourceRequest, NavigationError> {
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
}

pub(crate) fn is_http_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

struct LoadedResource {
    url: AppUrl,
    resource: Resource,
}

struct Page {
    document: Rc<RefCell<Document>>,
    runtime: ScriptRuntime,
    // parsed once per document, because layout and paint run on every frame
    stylesheet: Rc<Stylesheet>,
    // shared with the runtime, so script can lay out the document to measure it
    layout_inputs: Rc<RefCell<LayoutInputs>>,
    animated: bool,
    // the page clock time the last layout's animations next change what they show
    animation_wake: Cell<Option<u64>>,
    // decoded once per document and shared by every frame; failures are remembered too
    images: RefCell<HashMap<String, Option<Rc<Image>>>>,
}

enum HistoryCommit {
    Push { state: String },
    Replace { state: String },
    Traverse { index: usize, state: String },
}

pub struct Browser {
    resources: ResourceBundle,
    resource_provider: Option<Rc<dyn ResourceProvider>>,
    file_picker: Option<Rc<dyn FilePicker>>,
    clipboard_provider: Option<Rc<dyn ClipboardProvider>>,
    time_zone: Option<Rc<dyn crate::TimeZone>>,
    locale: Option<String>,
    last_navigation_failure: Option<NavigationFailure>,
    page: Option<Page>,
    current_url: Option<AppUrl>,
    history: Vec<AppUrl>,
    history_states: Vec<String>,
    history_document_ids: Vec<u64>,
    history_index: Option<usize>,
    current_document_id: Option<u64>,
    next_document_id: u64,
    local_storage: StorageHandle,
    session_storage: StorageHandle,
    hovered_node: Option<NodeId>,
    // the scrollbar thumb under the pointer, which ::-webkit-scrollbar-thumb:hover matches
    hovered_thumb: Option<NodeId>,
    // query container sizes from the last layout, which container queries read
    container_sizes:
        std::cell::RefCell<std::collections::BTreeMap<NodeId, crate::media::ContainerSize>>,
    // the pointer shape for the last pointer position
    cursor: crate::css::Cursor,
    transitions: BTreeMap<(NodeId, AnimatableProperty), TransitionState>,
    declared: BTreeMap<(NodeId, AnimatableProperty), String>,
    // each element's running keyframe animation, so it plays from when it was applied
    animations: BTreeMap<NodeId, AnimationRun>,
    motion_clock: Option<u64>,
    host_runtime_info: Option<HostRuntimeInfo>,
    host_bridge: Option<crate::script::HostBridge>,
    waker: Option<crate::bridge::Waker>,
    viewport: ViewportMetrics,
    preferences: UserPreferences,
    fonts: Rc<FontSet>,
    // the word or block a multi-click selected, which a following drag extends by
    selection_unit: Option<SelectionUnit>,
    // hit testing, cursor lookup and paint after one input all read the same layout
    layout_cache: RefCell<Option<(LayoutKey, Rc<LayoutDocument>)>>,
}

// everything a layout is a pure function of
#[derive(PartialEq)]
struct LayoutKey {
    document: (Option<u64>, *const RefCell<Document>, u64),
    stylesheet: *const crate::css::Stylesheet,
    viewport: Size,
    state: StyleState,
    selections: Vec<crate::layout::ProseSpan>,
}

#[derive(Clone, Copy)]
struct SelectionUnit {
    start: usize,
    end: usize,
    words: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UserPreferences {
    pub color_scheme: ColorScheme,
    pub reduced_motion: bool,
}

struct AnimationRun {
    name: String,
    started: u64,
    delay: u64,
    iteration: u64,
    began: bool,
    ended: bool,
}

// a transition or animation event waiting for dispatch once sampling releases the document
struct MotionEvent {
    node: NodeId,
    kind: &'static str,
    name: String,
    elapsed: f64,
}

impl MotionEvent {
    fn transition(
        (node, property): &(NodeId, AnimatableProperty),
        kind: &'static str,
        elapsed: u64,
    ) -> Self {
        Self {
            node: *node,
            kind,
            name: property.name().to_string(),
            elapsed: elapsed as f64 / 1000.0,
        }
    }

    fn animation(node: &NodeId, run: &AnimationRun, kind: &'static str, clock: u64) -> Self {
        Self {
            node: *node,
            kind,
            name: run.name.clone(),
            elapsed: clock.saturating_sub(run.started + run.delay) as f64 / 1000.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProseKey {
    Copy,
    SelectAll,
    Extend,
}

fn script_error(error: crate::script::ScriptError) -> NavigationError {
    NavigationError::Script(error.to_string())
}

impl Browser {
    pub fn new(resources: ResourceBundle) -> Self {
        Self {
            resources,
            resource_provider: None,
            file_picker: None,
            clipboard_provider: None,
            time_zone: None,
            locale: None,
            last_navigation_failure: None,
            page: None,
            current_url: None,
            history: Vec::new(),
            history_states: Vec::new(),
            history_document_ids: Vec::new(),
            history_index: None,
            current_document_id: None,
            next_document_id: 1,
            local_storage: Rc::new(RefCell::new(crate::script::StorageData::new())),
            session_storage: Rc::new(RefCell::new(crate::script::StorageData::new())),
            hovered_node: None,
            hovered_thumb: None,
            container_sizes: std::cell::RefCell::default(),
            cursor: crate::css::Cursor::Default,
            transitions: BTreeMap::new(),
            declared: BTreeMap::new(),
            animations: BTreeMap::new(),
            motion_clock: None,
            host_runtime_info: None,
            host_bridge: None,
            waker: None,
            viewport: ViewportMetrics::default(),
            preferences: UserPreferences::default(),
            fonts: Rc::new(FontSet::builtin()),
            selection_unit: None,
            layout_cache: RefCell::new(None),
        }
    }

    pub fn with_resource_provider(
        resources: ResourceBundle,
        resource_provider: Rc<dyn ResourceProvider>,
    ) -> Self {
        let mut browser = Self::new(resources);
        browser.resource_provider = Some(resource_provider);
        browser
    }

    pub fn with_storage_directory(mut self, directory: impl AsRef<Path>) -> io::Result<Self> {
        self.local_storage = Rc::new(RefCell::new(StorageData::load_from_directory(directory)?));
        Ok(self)
    }

    pub fn with_file_picker(mut self, file_picker: Rc<dyn FilePicker>) -> Self {
        self.file_picker = Some(file_picker);
        self
    }

    pub fn with_clipboard_provider(
        mut self,
        clipboard_provider: Rc<dyn ClipboardProvider>,
    ) -> Self {
        self.clipboard_provider = Some(clipboard_provider);
        self
    }

    // the local time zone scripts see through Date; without one, local time is UTC
    pub fn with_time_zone(mut self, time_zone: Rc<dyn crate::TimeZone>) -> Self {
        self.time_zone = Some(time_zone);
        self
    }

    // the default locale for Intl and navigator.language; without one, en-US
    pub fn with_locale(mut self, locale: String) -> Self {
        self.locale = Some(locale);
        self
    }

    pub fn with_host_runtime_info(mut self, info: HostRuntimeInfo) -> Self {
        self.host_runtime_info = Some(info);
        self
    }

    // gives each page a window.ceri that invokes commands through the router, as the
    // capabilities allow
    pub fn with_host_bridge(mut self, bridge: crate::script::HostBridge) -> Self {
        self.host_bridge = Some(bridge);
        self
    }

    // called from any thread when work for this browser arrives from outside it
    pub fn set_waker(&mut self, waker: crate::bridge::Waker) {
        self.waker = Some(waker);
    }

    // milliseconds until the page next has work of its own: a due timer, an animation
    // frame, or messages from its workers
    pub fn next_wake(&self) -> Option<u64> {
        let page = self.page.as_ref()?;
        let motion = (!self.transitions.is_empty() || page.runtime.has_live_workers())
            .then_some(FRAME_INTERVAL);
        // a stepped animation sleeps until its next step; a continuous one needs every frame
        let animation = page
            .animation_wake
            .get()
            .filter(|_| !self.preferences.reduced_motion)
            .map(|at| match at.saturating_sub(page.runtime.now()) {
                0 => FRAME_INTERVAL,
                delay => delay,
            });
        let task = page.runtime.next_task_delay();
        motion.into_iter().chain(animation).chain(task).min()
    }

    // an event from the host to the page's ceri.listen callbacks
    pub fn emit_event(&mut self, name: &str, payload: &str) -> Result<usize, NavigationError> {
        let delivered = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            page.runtime
                .dispatch_host_event(name, payload)
                .map_err(|error| NavigationError::Script(error.to_string()))?
        };
        self.process_pending_navigation()?;
        Ok(delivered)
    }

    pub fn set_viewport(&mut self, viewport: Size, device_pixel_ratio: f32) {
        self.viewport = ViewportMetrics {
            width: viewport.width.max(0.0),
            height: viewport.height.max(0.0),
            device_pixel_ratio: if device_pixel_ratio.is_finite() && device_pixel_ratio > 0.0 {
                device_pixel_ratio
            } else {
                1.0
            },
        };
        if let Some(page) = self.page.as_mut() {
            page.runtime.set_viewport_metrics(self.viewport);
        }
        self.evaluate_media_queries();
    }

    pub fn device_pixel_ratio(&self) -> f32 {
        self.viewport.device_pixel_ratio
    }

    pub fn with_fonts(mut self, fonts: Rc<FontSet>) -> Self {
        self.fonts = fonts;
        self
    }

    pub fn set_user_preferences(&mut self, preferences: UserPreferences) {
        self.preferences = preferences;
        if let Some(page) = self.page.as_ref() {
            let mut inputs = page.layout_inputs.borrow_mut();
            inputs.extras.color_scheme = preferences.color_scheme;
            inputs.extras.reduced_motion = preferences.reduced_motion;
        }
        self.evaluate_media_queries();
    }

    // media query lists answer again after the viewport or a preference changed
    fn evaluate_media_queries(&mut self) {
        if let Some(page) = self.page.as_mut()
            && let Err(error) = page.runtime.evaluate_media_queries()
        {
            page.runtime
                .report(format!("media query change listener failed: {error}"));
        }
    }

    fn layout_inputs(&self, stylesheet: Rc<Stylesheet>) -> Rc<RefCell<LayoutInputs>> {
        Rc::new(RefCell::new(LayoutInputs {
            stylesheet,
            fonts: self.fonts.clone(),
            extras: StyleState {
                color_scheme: self.preferences.color_scheme,
                reduced_motion: self.preferences.reduced_motion,
                ..StyleState::default()
            },
        }))
    }

    pub fn user_preferences(&self) -> UserPreferences {
        self.preferences
    }

    fn load_resource(&self, url: &AppUrl) -> Result<LoadedResource, NavigationError> {
        match self.resources.load(url) {
            Ok(resource) => Ok(LoadedResource {
                url: url.clone(),
                resource,
            }),
            Err(NavigationError::ResourceNotFound(path)) => {
                if let Some(provider) = &self.resource_provider
                    && let Some((loaded_url, resource)) =
                        load_provider_resource(provider.as_ref(), url)?
                {
                    return Ok(LoadedResource {
                        url: loaded_url,
                        resource,
                    });
                }
                if is_extensionless_path(url.path())
                    && let Some(provider) = &self.resource_provider
                {
                    let shell_url = url.with_path("/index.html")?;
                    if let Some((_, resource)) =
                        load_provider_resource(provider.as_ref(), &shell_url)?
                    {
                        return Ok(LoadedResource {
                            url: url.clone(),
                            resource,
                        });
                    }
                }
                Err(NavigationError::ResourceNotFound(path))
            }
            Err(error) => Err(error),
        }
    }

    fn load_navigation_request(
        &self,
        request: &ResourceRequest,
    ) -> Result<LoadedResource, NavigationError> {
        if request.method() == "GET" {
            return self.load_resource(request.url());
        }
        let Some(provider) = &self.resource_provider else {
            return Err(NavigationError::ResourceProvider(
                "non-GET navigation requires a resource provider".to_string(),
            ));
        };
        let Some((url, resource)) = load_provider_request_at(provider.as_ref(), request, 0)? else {
            return Err(NavigationError::ResourceNotFound(
                request.url().path().to_string(),
            ));
        };
        Ok(LoadedResource { url, resource })
    }

    fn load_subresource(&self, url: &AppUrl) -> Result<Resource, NavigationError> {
        self.load_subresource_with_url(url)
            .map(|loaded| loaded.resource)
    }

    fn load_subresource_with_url(&self, url: &AppUrl) -> Result<LoadedResource, NavigationError> {
        match self.resources.load_subresource(url) {
            Ok(resource) => Ok(LoadedResource {
                url: url.clone(),
                resource,
            }),
            Err(NavigationError::ResourceNotFound(path)) => {
                if let Some(provider) = &self.resource_provider
                    && let Some((loaded_url, resource)) =
                        load_provider_resource(provider.as_ref(), url)?
                {
                    return Ok(LoadedResource {
                        url: loaded_url,
                        resource,
                    });
                }
                Err(NavigationError::ResourceNotFound(path))
            }
            Err(error) => Err(error),
        }
    }

    pub fn navigate(&mut self, input: &str) -> Result<NavigationStatus, NavigationError> {
        let url = AppUrl::parse(input)?;
        self.navigate_url(
            url,
            HistoryCommit::Push {
                state: "null".to_string(),
            },
            0,
        )
    }

    fn navigate_url(
        &mut self,
        url: AppUrl,
        commit: HistoryCommit,
        redirect_depth: usize,
    ) -> Result<NavigationStatus, NavigationError> {
        self.navigate_request(ResourceRequest::get(url), commit, redirect_depth)
    }

    fn navigate_request(
        &mut self,
        request: ResourceRequest,
        commit: HistoryCommit,
        redirect_depth: usize,
    ) -> Result<NavigationStatus, NavigationError> {
        let requested_url = request.url().clone();
        let LoadedResource { url, resource } = match self.load_navigation_request(&request) {
            Ok(resource) => resource,
            Err(NavigationError::ResourceNotFound(path)) => {
                self.install_error_page(
                    &requested_url,
                    404,
                    &format!("resource not found: {path}"),
                );
                self.commit_new_document(requested_url, commit);
                return Ok(NavigationStatus::ErrorPage { status: 404 });
            }
            Err(error @ NavigationError::HttpStatus { .. }) => {
                let status = recovery_status(&error);
                self.install_error_page(&requested_url, status, &error.to_string());
                self.commit_new_document(requested_url, commit);
                return Ok(NavigationStatus::ErrorPage { status });
            }
            Err(error @ NavigationError::ResourceProvider(_)) => {
                self.install_error_page(&requested_url, 503, &error.to_string());
                self.commit_new_document(requested_url, commit);
                return Ok(NavigationStatus::ErrorPage { status: 503 });
            }
            Err(error) => return Err(error),
        };

        if !is_html(&resource) {
            self.install_error_page(
                &url,
                415,
                &format!("unsupported top-level resource: {}", resource.mime_type),
            );
            self.commit_new_document(url, commit);
            return Ok(NavigationStatus::ErrorPage { status: 415 });
        }

        let source = match String::from_utf8(resource.body) {
            Ok(source) => source,
            Err(error) => {
                self.install_error_page(&url, 500, &format!("HTML resource is not UTF-8: {error}"));
                self.commit_new_document(url, commit);
                return Ok(NavigationStatus::ErrorPage { status: 500 });
            }
        };
        let document = match Document::parse(&source) {
            Ok(document) => Rc::new(RefCell::new(document)),
            Err(error) => {
                self.install_error_page(&url, 500, &error.to_string());
                self.commit_new_document(url, commit);
                return Ok(NavigationStatus::ErrorPage { status: 500 });
            }
        };
        let stylesheet_sources = document.borrow().stylesheet_sources();
        let stylesheet = match self.load_stylesheets(&url, &stylesheet_sources) {
            Ok(stylesheet) => stylesheet,
            Err(error) => {
                let status = recovery_status(&error);
                self.install_error_page(&url, status, &error.to_string());
                self.commit_new_document(url, commit);
                return Ok(NavigationStatus::ErrorPage { status });
            }
        };
        let script_sources = document.borrow().script_sources();
        let mut runtime = ScriptRuntime::with_resources_and_storage_and_runtime_info(
            document.clone(),
            self.resources.clone(),
            self.resource_provider.clone(),
            url.clone(),
            self.local_storage.clone(),
            self.session_storage.clone(),
            self.host_runtime_info.clone(),
        );
        runtime.set_file_picker(self.file_picker.clone());
        runtime.set_clipboard_provider(self.clipboard_provider.clone());
        if let Some(bridge) = &self.host_bridge {
            runtime.set_host_bridge(bridge.clone(), APP_ORIGIN.to_string(), self.waker.clone());
        }
        if let Some(time_zone) = &self.time_zone {
            runtime.set_time_zone(time_zone.clone());
        }
        if let Some(locale) = &self.locale {
            runtime.set_locale(locale.clone());
        }
        runtime.set_viewport_metrics(self.viewport);
        let (parsed_stylesheet, dropped_rules) = Stylesheet::parse_recovering(&stylesheet);
        for dropped in dropped_rules {
            runtime.report(format!("dropped CSS rule: {dropped}"));
        }
        let parsed_stylesheet = Rc::new(parsed_stylesheet);
        self.load_web_fonts(&url, &parsed_stylesheet);
        let layout_inputs = self.layout_inputs(parsed_stylesheet.clone());
        runtime.set_layout_inputs(layout_inputs.clone());
        if let Err(error) = self.execute_scripts(&url, &script_sources, &mut runtime) {
            let status = recovery_status(&error);
            self.install_error_page(&url, status, &error.to_string());
            self.commit_new_document(url, commit);
            return Ok(NavigationStatus::ErrorPage { status });
        }
        if let Err(error) = runtime.apply_autofocus() {
            let error = NavigationError::Script(error.to_string());
            let status = recovery_status(&error);
            self.install_error_page(&url, status, &error.to_string());
            self.commit_new_document(url, commit);
            return Ok(NavigationStatus::ErrorPage { status });
        }
        self.persist_storage()?;

        let pending_navigation = runtime.take_navigation_request();
        let animated = parsed_stylesheet.has_animations();
        self.page = Some(Page {
            document,
            runtime,
            stylesheet: parsed_stylesheet,
            layout_inputs,
            animated,
            animation_wake: Cell::new(None),
            images: RefCell::default(),
        });
        self.last_navigation_failure = None;
        self.hovered_node = None;
        self.hovered_thumb = None;
        self.commit_new_document(url, commit);
        if let Some(request) = pending_navigation {
            return self.follow_navigation(request, redirect_depth);
        }
        Ok(NavigationStatus::Loaded)
    }

    pub fn current_url(&self) -> Option<&AppUrl> {
        self.current_url.as_ref()
    }

    pub fn last_navigation_failure(&self) -> Option<&NavigationFailure> {
        self.last_navigation_failure.as_ref()
    }

    // a page with a running animation or transition needs a frame per clock tick
    pub fn has_active_animations(&self) -> bool {
        if self.preferences.reduced_motion {
            return false;
        }
        let animating = self
            .page
            .as_ref()
            .is_some_and(|page| page.animation_wake.get().is_some());
        animating || !self.transitions.is_empty()
    }

    // records which animatable values changed, so layout can blend from the old ones
    pub fn sample_transitions(&mut self, viewport: Size) -> Result<bool, NavigationError> {
        let Some(page) = self.page.as_ref() else {
            return Ok(false);
        };
        let has_motion = page.stylesheet.has_transitions() || page.animated;
        if !has_motion || self.preferences.reduced_motion {
            self.transitions.clear();
            self.declared.clear();
            self.animations.clear();
            return Ok(false);
        }
        let clock = page.runtime.now();
        let previous_clock = self.motion_clock.replace(clock);
        let document = page.document.clone();
        let document = document.borrow();
        let mut state = page.runtime.style_state(self.hovered_node);
        state.viewport_width = viewport.width;
        state.viewport_height = viewport.height;
        state.color_scheme = self.preferences.color_scheme;
        state.reduced_motion = self.preferences.reduced_motion;
        state.clock = clock;

        let mut seen = BTreeSet::new();
        let mut seen_animations = BTreeSet::new();
        let mut events = Vec::new();
        let mut changed = false;
        let candidates = collect_motion_candidates(&page.stylesheet, &state, &document);
        // custom properties cost a document-wide pass, so only pages with moving elements pay it
        if !candidates.is_empty() {
            page.stylesheet
                .populate_custom_properties(&document, &mut state);
        }
        for node in candidates {
            let values = page.stylesheet.declared_values(StyleContext {
                document: &document,
                node,
                pseudo: None,
                parent_font_size: 16.0,
                parent_color: crate::css::Color::BLACK,
                parent_weight: crate::css::FontWeight::NORMAL,
                parent_text: Default::default(),
                state: &state,
            });
            for transition in crate::css::transitions_from(&values).into_iter().flatten() {
                for property in transition.property.expand() {
                    let Some(value) = values.get(property.name()) else {
                        continue;
                    };
                    let key = (node, property);
                    seen.insert(key);
                    match self.declared.get(&key) {
                        Some(previous) if previous == value => {}
                        Some(previous) => {
                            // a change while a transition runs replaces it
                            if let Some(running) = self.transitions.get(&key)
                                && clock <= running.finishes
                            {
                                events.push(MotionEvent::transition(
                                    &key,
                                    "transitioncancel",
                                    clock.saturating_sub(running.started + running.delay),
                                ));
                            }
                            if transition.duration > 0 {
                                self.transitions.insert(
                                    key,
                                    TransitionState {
                                        from: previous.clone(),
                                        to: value.clone(),
                                        started: clock,
                                        finishes: clock
                                            .saturating_add(transition.delay)
                                            .saturating_add(transition.duration),
                                        delay: transition.delay,
                                        duration: transition.duration,
                                        timing: transition.timing,
                                    },
                                );
                                events.push(MotionEvent::transition(&key, "transitionrun", 0));
                                if transition.delay == 0 {
                                    events.push(MotionEvent::transition(
                                        &key,
                                        "transitionstart",
                                        0,
                                    ));
                                }
                                changed = true;
                            } else {
                                self.transitions.remove(&key);
                            }
                        }
                        None => {}
                    }
                    self.declared.insert(key, value.clone());
                }
            }
            if let Some(animation) = crate::css::animation_from(&values)
                .filter(|animation| page.stylesheet.has_keyframes(&animation.name))
            {
                seen_animations.insert(node);
                if self
                    .animations
                    .get(&node)
                    .is_some_and(|run| run.name != animation.name)
                    && let Some(replaced) = self.animations.remove(&node)
                    && !replaced.ended
                {
                    events.push(MotionEvent::animation(
                        &node,
                        &replaced,
                        "animationcancel",
                        clock,
                    ));
                }
                let run = self.animations.entry(node).or_insert_with(|| AnimationRun {
                    name: animation.name.clone(),
                    started: clock,
                    delay: animation.delay,
                    iteration: 0,
                    began: false,
                    ended: false,
                });
                let elapsed = clock.saturating_sub(run.started);
                if !run.began && elapsed >= animation.delay {
                    run.began = true;
                    events.push(MotionEvent::animation(
                        &node,
                        run,
                        "animationstart",
                        run.started + run.delay,
                    ));
                }
                if run.began && !run.ended && animation.duration > 0 {
                    let active = elapsed.saturating_sub(animation.delay);
                    let current = active / animation.duration;
                    let finished = animation.iterations.is_some_and(|iterations| {
                        active as f32 >= iterations * animation.duration as f32
                    });
                    if finished {
                        run.ended = true;
                        events.push(MotionEvent::animation(&node, run, "animationend", clock));
                    } else if current > run.iteration {
                        run.iteration = current;
                        events.push(MotionEvent::animation(
                            &node,
                            run,
                            "animationiteration",
                            clock,
                        ));
                    }
                }
            }
        }
        // starts after a delay and ends are noticed as the clock passes them
        for (key, active) in &self.transitions {
            let start = active.started + active.delay;
            if active.delay > 0
                && previous_clock.is_some_and(|previous| previous < start)
                && start <= clock
            {
                events.push(MotionEvent::transition(key, "transitionstart", 0));
            }
            if clock > active.finishes {
                let name = if seen.contains(key) {
                    "transitionend"
                } else {
                    "transitioncancel"
                };
                events.push(MotionEvent::transition(key, name, active.duration));
            } else if !seen.contains(key) {
                events.push(MotionEvent::transition(
                    key,
                    "transitioncancel",
                    clock.saturating_sub(start),
                ));
            }
        }
        // a finished transition is dropped, so the page stops asking for frames
        self.transitions
            .retain(|key, active| seen.contains(key) && clock <= active.finishes);
        self.declared.retain(|key, _| seen.contains(key));
        let removed = self
            .animations
            .iter()
            .filter(|(node, run)| !seen_animations.contains(*node) && !run.ended)
            .map(|(node, run)| MotionEvent::animation(node, run, "animationcancel", clock))
            .collect::<Vec<_>>();
        events.extend(removed);
        self.animations
            .retain(|node, _| seen_animations.contains(node));
        drop(document);
        if events.is_empty() {
            return Ok(changed);
        }
        let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
        for event in events {
            page.runtime
                .dispatch_motion_event(event.node, event.kind, &event.name, event.elapsed)
                .map_err(script_error)?;
        }
        self.process_pending_navigation()?;
        Ok(true)
    }

    // shows the engine's own recovery document, which a host uses after a failed restart
    pub fn show_recovery(&mut self, status: u16, detail: &str) {
        let url = self
            .current_url
            .clone()
            .or_else(|| AppUrl::parse(&format!("{APP_ORIGIN}/")).ok());
        let Some(url) = url else {
            return;
        };
        self.install_error_page(&url, status, detail);
    }

    pub fn control_value(&self, node: NodeId) -> Option<String> {
        self.page
            .as_ref()
            .and_then(|page| page.runtime.control_value(node))
    }

    pub fn title(&self) -> Option<String> {
        self.page
            .as_ref()
            .and_then(|page| page.document.borrow().title())
    }

    pub fn accessibility_tree(&self) -> Option<AccessibilityNode> {
        self.page
            .as_ref()
            .map(|page| page.document.borrow().accessibility_tree())
    }

    pub fn history(&self) -> &[AppUrl] {
        &self.history
    }

    pub fn go(&mut self, delta: isize) -> Result<bool, NavigationError> {
        let before = self.history_index;
        self.traverse_history(delta)?;
        Ok(before != self.history_index)
    }

    pub fn back(&mut self) -> Result<bool, NavigationError> {
        self.go(-1)
    }

    pub fn forward(&mut self) -> Result<bool, NavigationError> {
        self.go(1)
    }

    pub fn document(&self) -> Result<Rc<RefCell<Document>>, NavigationError> {
        self.page
            .as_ref()
            .map(|page| page.document.clone())
            .ok_or(NavigationError::NoPage)
    }

    pub fn click_id(&mut self, id: &str) -> Result<bool, NavigationError> {
        let node = {
            let page = self.page.as_ref().ok_or(NavigationError::NoPage)?;
            page.document.borrow().find_element_by_id(id)
        };
        let Some(node) = node else {
            return Ok(false);
        };
        self.activate_node(node)
    }

    pub fn click_at(&mut self, viewport: Size, x: f32, y: f32) -> Result<bool, NavigationError> {
        self.click_at_with_count(viewport, x, y, 1)
    }

    // a host click that is the nth in a rapid series; the second also fires dblclick
    pub fn click_at_with_count(
        &mut self,
        viewport: Size,
        x: f32,
        y: f32,
        count: u32,
    ) -> Result<bool, NavigationError> {
        let node = self.layout(viewport)?.hit_test(x, y);
        let Some(node) = node else {
            return Ok(false);
        };
        let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
        page.runtime.set_pointer_position(x, y);
        page.runtime.set_click_detail(count);
        let clicked = self.activate_node(node);
        let double = match self.page.as_mut() {
            Some(page)
                if count == 2 && clicked.is_ok() && page.document.borrow().is_connected(node) =>
            {
                page.runtime
                    .dispatch_node_event(node, "dblclick")
                    .map_err(script_error)
            }
            _ => Ok(false),
        };
        if let Some(page) = self.page.as_mut() {
            page.runtime.set_click_detail(1);
        }
        Ok(clicked? | double?)
    }

    pub fn scroll_at(
        &mut self,
        viewport: Size,
        x: f32,
        y: f32,
        delta_x: f32,
        delta_y: f32,
    ) -> Result<bool, NavigationError> {
        let Some(target) = self.layout(viewport)?.scroll_target_at(x, y) else {
            return Ok(false);
        };
        let delta_x = if delta_x.is_finite() { delta_x } else { 0.0 };
        let delta_y = if delta_y.is_finite() { delta_y } else { 0.0 };
        let offset = (
            (target.offset.0 + delta_x).clamp(0.0, target.max_x),
            (target.offset.1 + delta_y).clamp(0.0, target.max_y),
        );
        if offset == target.offset {
            return Ok(false);
        }
        let changed = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            let changed = page.runtime.set_scroll_offset(target.node, offset);
            if changed {
                if target.node == page.document.borrow().root() {
                    page.runtime
                        .dispatch_window_event("scroll")
                        .map_err(|error| NavigationError::Script(error.to_string()))?;
                } else {
                    page.runtime
                        .dispatch_node_event(target.node, "scroll")
                        .map_err(|error| NavigationError::Script(error.to_string()))?;
                }
            }
            changed
        };
        if self.process_pending_navigation()?.is_some() {
            Ok(true)
        } else {
            Ok(changed)
        }
    }

    // returns the scrollbar thumb drag state under a pointer press
    pub fn thumb_at(&self, viewport: Size, x: f32, y: f32) -> Option<ThumbDrag> {
        let _fonts = crate::font::use_fonts(self.fonts.clone());
        self.layout(viewport).ok()?.thumb_drag_at(x, y)
    }

    // moves a scrollbar thumb so the pointer stays grabbed at its press offset
    pub fn drag_thumb(
        &mut self,
        drag: &ThumbDrag,
        x: f32,
        y: f32,
    ) -> Result<bool, NavigationError> {
        // the thumb keeps the pointer's offset inside it from the press, so it
        // does not jump to center under the pointer
        let travel = (if drag.vertical {
            drag.track.height
        } else {
            drag.track.width
        } - drag.thumb_extent)
            .max(0.0);

        let pointer_along = (if drag.vertical { y } else { x })
            - if drag.vertical {
                drag.track.y
            } else {
                drag.track.x
            }
            - drag.grab;
        let offset_along = if travel > 0.0 && drag.max_offset > 0.0 {
            (pointer_along / travel).clamp(0.0, 1.0) * drag.max_offset
        } else if drag.vertical {
            drag.offset.1
        } else {
            drag.offset.0
        };
        self.scroll_from_bar(drag, offset_along)
    }

    // a press on a bar off its thumb scrolls a page or a line toward the pointer
    pub(crate) fn page_by_bar(&mut self, drag: &ThumbDrag) -> Result<bool, NavigationError> {
        let along = if drag.vertical {
            drag.offset.1
        } else {
            drag.offset.0
        };
        self.scroll_from_bar(drag, (along + drag.page).clamp(0.0, drag.max_offset))
    }

    fn scroll_from_bar(&mut self, drag: &ThumbDrag, along: f32) -> Result<bool, NavigationError> {
        let offset = if drag.vertical {
            (drag.offset.0, along)
        } else {
            (along, drag.offset.1)
        };
        if offset == drag.offset {
            return Ok(false);
        }
        let changed = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            let changed = page.runtime.set_scroll_offset(drag.node, offset);
            if changed {
                if drag.node == page.document.borrow().root() {
                    page.runtime
                        .dispatch_window_event("scroll")
                        .map_err(|error| NavigationError::Script(error.to_string()))?;
                } else {
                    page.runtime
                        .dispatch_node_event(drag.node, "scroll")
                        .map_err(|error| NavigationError::Script(error.to_string()))?;
                }
            }
            changed
        };
        if self.process_pending_navigation()?.is_some() {
            Ok(true)
        } else {
            Ok(changed)
        }
    }

    // a press on a range input starts dragging its thumb, which centers under a press on the
    // track at once and fires input
    pub(crate) fn press_range(
        &mut self,
        viewport: Size,
        x: f32,
        y: f32,
    ) -> Result<Option<RangeDrag>, NavigationError> {
        let drag = {
            let _fonts = crate::font::use_fonts(self.fonts.clone());
            self.layout(viewport)?.range_drag_at(x, y)
        };
        let Some(mut drag) = drag else {
            return Ok(None);
        };
        let page = self.page.as_ref().ok_or(NavigationError::NoPage)?;
        drag.pressed = page.runtime.control_value(drag.node).unwrap_or_default();
        self.drag_range(&drag, x)?;
        Ok(Some(drag))
    }

    pub(crate) fn drag_range(&mut self, drag: &RangeDrag, x: f32) -> Result<bool, NavigationError> {
        let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
        let changed = page
            .runtime
            .drag_range(drag.node, drag.fraction(x))
            .map_err(|error| NavigationError::Script(error.to_string()))?;
        Ok(self.process_pending_navigation()?.is_some() || changed)
    }

    // the drag ended; change fires if the value moved since the press
    pub(crate) fn release_range(&mut self, drag: &RangeDrag) -> Result<(), NavigationError> {
        let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
        page.runtime
            .release_range(drag.node, &drag.pressed)
            .map_err(|error| NavigationError::Script(error.to_string()))?;
        self.process_pending_navigation()?;
        Ok(())
    }

    // a pointer press or drag over a text control or prose moves or extends its selection;
    // a press that lands on neither clears the document selection
    pub fn select_at(
        &mut self,
        viewport: Size,
        x: f32,
        y: f32,
        extend: bool,
    ) -> Result<bool, NavigationError> {
        let layout = {
            let _fonts = crate::font::use_fonts(self.fonts.clone());
            self.layout(viewport)?
        };
        if !extend {
            self.selection_unit = None;
        }
        if let Some((node, offset)) = layout.control_text_offset_at(x, y) {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            return page
                .runtime
                .select_control_offset(node, offset, extend)
                .map_err(script_error);
        }
        let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
        let Some((node, offset)) = layout.prose_text_offset_at(x, y, extend) else {
            if !extend {
                page.runtime
                    .clear_document_selection()
                    .map_err(script_error)?;
            }
            return Ok(false);
        };
        let Some(unit) = self.selection_unit.filter(|_| extend) else {
            return page
                .runtime
                .select_prose_offset(node, offset, extend)
                .map_err(script_error);
        };
        // a drag after a multi-click grows by whole units from the clicked one
        let (anchor, focus) = {
            let document = page.document.borrow();
            let Some(position) = document.node_text_offset(node).map(|start| start + offset) else {
                return Ok(false);
            };
            let (word_start, word_end) = if unit.words {
                crate::selection::word_around(&document, node, offset)
                    .unwrap_or((position, position))
            } else {
                (position, position)
            };
            if position < unit.start {
                (unit.end, word_start)
            } else {
                (unit.start, word_end.max(unit.end))
            }
        };
        page.runtime
            .select_document_points(anchor, focus, false)
            .map_err(script_error)?;
        Ok(true)
    }

    // a double click selects the word under the pointer, a triple click its block
    pub fn select_unit_at(
        &mut self,
        viewport: Size,
        x: f32,
        y: f32,
        clicks: u32,
    ) -> Result<bool, NavigationError> {
        let layout = {
            let _fonts = crate::font::use_fonts(self.fonts.clone());
            self.layout(viewport)?
        };
        let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
        if let Some((node, offset)) = layout.control_text_offset_at(x, y) {
            let value = page.runtime.control_value(node).unwrap_or_default();
            let units = value.encode_utf16().collect::<Vec<_>>();
            let (start, end) = if clicks == 2 {
                crate::text::word_at(&units, offset)
            } else {
                (0, units.len())
            };
            return page
                .runtime
                .select_control_range(node, start, end)
                .map_err(script_error);
        }
        let Some((node, offset)) = layout.prose_text_offset_at(x, y, false) else {
            return Ok(false);
        };
        let bounds = {
            let document = page.document.borrow();
            if clicks == 2 {
                crate::selection::word_around(&document, node, offset)
            } else {
                document
                    .node_text_offset(node)
                    .map(|start| start + offset)
                    .and_then(|position| {
                        crate::selection::ProseText::new(&layout, &document).block_around(position)
                    })
            }
        };
        let Some((start, end)) = bounds else {
            return Ok(false);
        };
        self.selection_unit = Some(SelectionUnit {
            start,
            end,
            words: clicks == 2,
        });
        page.runtime
            .select_document_points(start, end, true)
            .map_err(script_error)?;
        Ok(true)
    }

    // the selected prose as a reader sees it, for the clipboard
    pub fn selected_text(&self, viewport: Size) -> Result<String, NavigationError> {
        let page = self.page.as_ref().ok_or(NavigationError::NoPage)?;
        let Some((anchor, focus)) = page.runtime.document_selection_points() else {
            return Ok(String::new());
        };
        let layout = {
            let _fonts = crate::font::use_fonts(self.fonts.clone());
            self.layout(viewport)?
        };
        let document = page.document.borrow();
        Ok(crate::selection::ProseText::new(&layout, &document)
            .text_between(anchor.min(focus), anchor.max(focus)))
    }

    // copy, select-all and keyboard extension over prose while no text control has focus
    fn prose_key(
        &mut self,
        key: &str,
        modifiers: KeyModifiers,
    ) -> Result<Option<bool>, NavigationError> {
        let command = modifiers.control || modifiers.meta;
        let by_word = crate::script::word_modifier(modifiers);
        let page = self.page.as_ref().ok_or(NavigationError::NoPage)?;
        if (modifiers.alt && !by_word) || page.runtime.focused_text_control() {
            return Ok(None);
        }
        let points = page.runtime.document_selection_points();
        let action = match key {
            "c" | "C" if command && points.is_some_and(|(anchor, focus)| anchor != focus) => {
                ProseKey::Copy
            }
            "a" | "A" if command => ProseKey::SelectAll,
            "ArrowLeft" | "ArrowRight" | "Home" | "End" if modifiers.shift && points.is_some() => {
                ProseKey::Extend
            }
            _ => return Ok(None),
        };
        let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
        let dispatch = page
            .runtime
            .dispatch_shortcut_keydown(key, modifiers)
            .map_err(script_error)?;
        if dispatch.default_prevented {
            return Ok(Some(dispatch.handled));
        }
        let viewport = Size {
            width: self.viewport.width,
            height: self.viewport.height,
        };
        if action == ProseKey::Copy {
            let text = self.selected_text(viewport)?;
            if let Some(clipboard) = &self.clipboard_provider {
                clipboard.write_text(&text)?;
            }
            return Ok(Some(true));
        }
        let layout = {
            let _fonts = crate::font::use_fonts(self.fonts.clone());
            self.layout(viewport)?
        };
        let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
        let selection = {
            let prose = crate::selection::ProseText::new(&layout, &page.document.borrow());
            match (action, points) {
                (ProseKey::SelectAll, _) => prose.bounds(),
                (_, Some((anchor, focus))) => {
                    let forward = matches!(key, "ArrowRight" | "End");
                    // Cmd with an arrow moves to the line edge on macOS
                    let focus = match key {
                        "Home" | "End" => prose.line_edge(focus, forward),
                        _ if by_word => prose.step_word(focus, forward),
                        _ if modifiers.meta && cfg!(target_os = "macos") => {
                            prose.line_edge(focus, forward)
                        }
                        _ => prose.step_character(focus, forward),
                    };
                    Some((anchor, focus))
                }
                _ => None,
            }
        };
        let Some((anchor, focus)) = selection else {
            return Ok(Some(dispatch.handled));
        };
        self.selection_unit = None;
        page.runtime
            .select_document_points(anchor, focus, action == ProseKey::SelectAll)
            .map_err(script_error)?;
        Ok(Some(true))
    }

    // a press or release of button 0 (primary), 1 (auxiliary) or 2 (secondary), which the
    // next pointer events report
    pub fn set_pointer_button(&mut self, button: i16, pressed: bool) {
        if let Some(page) = self.page.as_mut() {
            page.runtime.set_pointer_button(button, pressed);
        }
    }

    pub fn cursor(&self) -> crate::css::Cursor {
        self.cursor
    }

    pub fn dispatch_at(
        &mut self,
        viewport: Size,
        x: f32,
        y: f32,
        event: &str,
    ) -> Result<bool, NavigationError> {
        let layout = self.layout(viewport)?;
        let node = layout.hit_test(x, y);
        let moved_cursor = (event == "pointermove").then(|| layout.cursor_at(x, y));
        let thumb_changed = event == "pointermove" && {
            let thumb = layout
                .thumb_drag_at(x, y)
                .filter(|drag| drag.page == 0.0)
                .map(|drag| drag.node);
            std::mem::replace(&mut self.hovered_thumb, thumb) != thumb
        };
        let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
        page.runtime.set_pointer_position(x, y);
        let hover_changed = if event == "pointermove" && self.hovered_node != node {
            let previous = std::mem::replace(&mut self.hovered_node, node);
            page.runtime
                .dispatch_hover_change(previous, node)
                .map_err(|error| NavigationError::Script(error.to_string()))?;
            true
        } else {
            thumb_changed
        };
        if let Some(cursor) = moved_cursor {
            // hover rules can change the cursor, so a hover change reads it from the new styles
            self.cursor = if hover_changed {
                self.layout(viewport)?.cursor_at(x, y)
            } else {
                cursor
            };
        }
        let Some(node) = node else {
            return Ok(hover_changed);
        };
        let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
        let handled = {
            let focused = if event == "pointerdown" {
                page.runtime.note_pointer_press();
                page.runtime
                    .focus_node(node)
                    .map_err(|error| NavigationError::Script(error.to_string()))?
            } else {
                false
            };
            let dispatched = page
                .runtime
                .dispatch_pointer_event(node, event)
                .map_err(|error| NavigationError::Script(error.to_string()))?;
            focused || dispatched
        };
        if self.process_pending_navigation()?.is_some() {
            Ok(true)
        } else {
            Ok(handled || hover_changed)
        }
    }

    pub fn key_down(&mut self, key: &str) -> Result<bool, NavigationError> {
        self.key_down_with_modifiers(key, KeyModifiers::default())
    }

    pub fn key_down_with_modifiers(
        &mut self,
        key: &str,
        modifiers: KeyModifiers,
    ) -> Result<bool, NavigationError> {
        self.key_down_with_code(key, None, modifiers, false)
    }

    // a keydown with the physical key code and auto-repeat flag, when the window reports them
    pub fn key_down_with_code(
        &mut self,
        key: &str,
        code: Option<&str>,
        modifiers: KeyModifiers,
        repeat: bool,
    ) -> Result<bool, NavigationError> {
        let handled = if let Some(handled) = self.prose_key(key, modifiers)? {
            handled
        } else {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            match code {
                Some(code) => page
                    .runtime
                    .key_down_with_code(key, code, modifiers, repeat),
                None => page.runtime.key_down_with_modifiers(key, modifiers),
            }
            .map_err(|error| NavigationError::Script(error.to_string()))?
        };
        if self.process_pending_navigation()?.is_some() {
            Ok(true)
        } else {
            Ok(handled)
        }
    }

    pub fn key_up(
        &mut self,
        key: &str,
        code: &str,
        modifiers: KeyModifiers,
    ) -> Result<bool, NavigationError> {
        let handled = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            page.runtime
                .key_up(key, code, modifiers)
                .map_err(|error| NavigationError::Script(error.to_string()))?
        };
        if self.process_pending_navigation()?.is_some() {
            Ok(true)
        } else {
            Ok(handled)
        }
    }

    pub fn insert_text(&mut self, text: &str) -> Result<bool, NavigationError> {
        let handled = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            page.runtime
                .insert_text(text)
                .map_err(|error| NavigationError::Script(error.to_string()))?
        };
        if self.process_pending_navigation()?.is_some() {
            Ok(true)
        } else {
            Ok(handled)
        }
    }

    pub fn composition_update(
        &mut self,
        text: &str,
        cursor: Option<(usize, usize)>,
    ) -> Result<bool, NavigationError> {
        let handled = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            page.runtime
                .composition_update(text, cursor)
                .map_err(|error| NavigationError::Script(error.to_string()))?
        };
        if self.process_pending_navigation()?.is_some() {
            Ok(true)
        } else {
            Ok(handled)
        }
    }

    pub fn composition_commit(&mut self, text: &str) -> Result<bool, NavigationError> {
        let handled = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            page.runtime
                .composition_commit(text)
                .map_err(|error| NavigationError::Script(error.to_string()))?
        };
        if self.process_pending_navigation()?.is_some() {
            Ok(true)
        } else {
            Ok(handled)
        }
    }

    pub fn composition_cancel(&mut self) -> Result<bool, NavigationError> {
        let handled = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            page.runtime
                .composition_cancel()
                .map_err(|error| NavigationError::Script(error.to_string()))?
        };
        if self.process_pending_navigation()?.is_some() {
            Ok(true)
        } else {
            Ok(handled)
        }
    }

    pub fn blur(&mut self) -> Result<bool, NavigationError> {
        let handled = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            page.runtime
                .blur_focused()
                .map_err(|error| NavigationError::Script(error.to_string()))?
        };
        if self.process_pending_navigation()?.is_some() {
            Ok(true)
        } else {
            Ok(handled)
        }
    }

    pub fn advance_time(&mut self, milliseconds: u64) -> Result<usize, NavigationError> {
        let executed = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            page.runtime
                .advance_time(milliseconds)
                .map_err(|error| NavigationError::Script(error.to_string()))?
        };
        self.process_pending_navigation()?;
        Ok(executed)
    }

    pub fn pump(&mut self) -> Result<usize, NavigationError> {
        let processed = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            page.runtime
                .pump()
                .map_err(|error| NavigationError::Script(error.to_string()))?
        };
        self.process_pending_navigation()?;
        Ok(processed)
    }

    pub fn console_logs(&self) -> &[String] {
        self.page
            .as_ref()
            .map(|page| page.runtime.logs())
            .unwrap_or(&[])
    }

    pub fn layout(&self, viewport: Size) -> Result<Rc<LayoutDocument>, NavigationError> {
        let page = self.page.as_ref().ok_or(NavigationError::NoPage)?;
        let document = page.document.clone();
        let document = document.borrow();
        let stylesheet = &page.stylesheet;
        let mut style_state = page.runtime.style_state(self.hovered_node);
        style_state.hovered_part = self
            .hovered_thumb
            .map(|node| (node, crate::css::PseudoElement::ScrollbarThumb));
        style_state.viewport_width = viewport.width;
        style_state.viewport_height = viewport.height;
        style_state.color_scheme = self.preferences.color_scheme;
        style_state.reduced_motion = self.preferences.reduced_motion;
        style_state.clock = page.runtime.now();
        style_state.transitions = self.transitions.clone();
        style_state.animation_starts = self
            .animations
            .iter()
            .map(|(node, run)| (*node, run.started))
            .collect();
        let _fonts = crate::font::use_fonts(self.fonts.clone());
        if let Some(base) = self.current_url.as_ref() {
            for node in document
                .find_elements_by_selector("img")
                .unwrap_or_default()
            {
                let image = document
                    .attribute(node, "src")
                    .and_then(|source| self.image_for(page, base, source));
                if let Some(image) = image {
                    style_state
                        .image_sizes
                        .insert(node, (image.width, image.height));
                }
            }
        }
        style_state.container_sizes = self.container_sizes.borrow().clone();
        let mut keyed_state = style_state.clone();
        // a page with nothing in motion lays out the same at any clock time
        if page.animation_wake.get().is_none() && keyed_state.transitions.is_empty() {
            keyed_state.clock = 0;
        }
        let key = LayoutKey {
            document: (
                self.current_document_id,
                Rc::as_ptr(&page.document),
                document.version(),
            ),
            stylesheet: Rc::as_ptr(stylesheet),
            viewport,
            state: keyed_state,
            selections: page.runtime.prose_selections(),
        };
        if let Some((cached, layout)) = self.layout_cache.borrow().as_ref()
            && *cached == key
        {
            return Ok(layout.clone());
        }
        crate::css::take_animation_wake();
        let mut layout = build_layout(&document, stylesheet, viewport, &style_state);
        // container queries read sizes from a layout, so a change in them lays out once more
        if stylesheet.has_container_queries() {
            let measured = crate::layout::take_layout_containers();
            if measured != style_state.container_sizes {
                style_state.container_sizes = measured;
                layout = build_layout(&document, stylesheet, viewport, &style_state);
            }
            *self.container_sizes.borrow_mut() = style_state.container_sizes.clone();
        }
        page.animation_wake.set(
            crate::css::take_animation_wake().map(|delay| style_state.clock.saturating_add(delay)),
        );
        page.layout_inputs.borrow_mut().extras = StyleState {
            hovered: self.hovered_node,
            color_scheme: style_state.color_scheme,
            reduced_motion: style_state.reduced_motion,
            transitions: style_state.transitions.clone(),
            animation_starts: style_state.animation_starts.clone(),
            image_sizes: style_state.image_sizes.clone(),
            container_sizes: style_state.container_sizes.clone(),
            ..StyleState::default()
        };
        layout.prose_selections = key.selections.clone();
        if let Some(base) = self.current_url.as_ref() {
            self.attach_image_layers(page, base, &document, &mut layout);
        }
        let layout = Rc::new(layout);
        *self.layout_cache.borrow_mut() = Some((key, layout.clone()));
        Ok(layout)
    }

    pub fn paint(&self, viewport: Size) -> Result<Canvas, NavigationError> {
        self.paint_scaled(viewport, 1.0)
    }

    pub fn paint_scaled(
        &self,
        viewport: Size,
        device_pixel_ratio: f32,
    ) -> Result<Canvas, NavigationError> {
        let layout = self.layout(viewport)?;
        let commands = self
            .page
            .as_ref()
            .map(|page| page.runtime.canvas_commands())
            .unwrap_or(&[]);
        let _fonts = crate::font::use_fonts(self.fonts.clone());
        Ok(crate::paint::paint_with_commands(
            &layout,
            commands,
            device_pixel_ratio,
        ))
    }

    pub fn describe(&self, viewport: Size) -> Result<String, NavigationError> {
        Ok(self.layout(viewport)?.describe())
    }

    fn attach_image_layers(
        &self,
        page: &Page,
        base: &AppUrl,
        document: &Document,
        layout: &mut LayoutDocument,
    ) {
        let mut sources = Vec::new();
        collect_image_sources(&layout.root, &mut sources);
        for (node, background) in sources {
            let reference = match background {
                Some(source) => crate::image::image_source_url(source).map(|url| url.to_string()),
                None => document.attribute(node, "src").map(str::to_string),
            };
            let Some(image) =
                reference.and_then(|reference| self.image_for(page, base, &reference))
            else {
                continue;
            };
            layout.images.push(ImageLayer {
                node,
                image,
                background,
            });
        }
    }

    // decodes an image reference once per document, from data: URLs or app-origin resources
    fn image_for(&self, page: &Page, base: &AppUrl, reference: &str) -> Option<Rc<Image>> {
        let reference = reference.trim();
        let key = if reference.len() > 5 && reference[..5].eq_ignore_ascii_case("data:") {
            reference.to_string()
        } else {
            base.resolve(reference).ok()?.to_string()
        };
        if let Some(cached) = page.images.borrow().get(&key) {
            return cached.clone();
        }
        let decoded = if let Some((mime_type, bytes)) = crate::image::decode_data_url(&key) {
            crate::image::decode_image(&bytes, &mime_type).ok()
        } else {
            base.resolve(reference)
                .ok()
                .and_then(|url| self.load_subresource(&url).ok())
                .and_then(|resource| {
                    crate::image::decode_image(resource.body(), &resource.mime_type).ok()
                })
        }
        .map(Rc::new);
        page.images.borrow_mut().insert(key, decoded.clone());
        decoded
    }

    fn activate_node(&mut self, node: NodeId) -> Result<bool, NavigationError> {
        let handled = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            page.runtime
                .click_node(node)
                .map_err(|error| NavigationError::Script(error.to_string()))?
        };
        if self.process_pending_navigation()?.is_some() {
            Ok(true)
        } else {
            Ok(handled)
        }
    }

    fn process_pending_navigation(&mut self) -> Result<Option<NavigationStatus>, NavigationError> {
        self.persist_storage()?;
        let request = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            page.runtime.take_navigation_request()
        };
        request
            .map(|request| self.follow_navigation(request, 0))
            .transpose()
    }

    fn persist_storage(&self) -> Result<(), NavigationError> {
        self.local_storage
            .borrow()
            .persist()
            .map_err(|error| NavigationError::Storage(error.to_string()))
    }

    fn follow_navigation(
        &mut self,
        request: NavigationRequest,
        redirect_depth: usize,
    ) -> Result<NavigationStatus, NavigationError> {
        if redirect_depth >= MAX_NAVIGATION_REDIRECTS {
            return Err(NavigationError::NavigationLoop);
        }
        match request {
            NavigationRequest::Navigate {
                reference,
                replace_history,
            } => {
                let base = self.current_url.clone().ok_or(NavigationError::NoPage)?;
                let url = base.resolve(&reference)?;
                let commit = if replace_history {
                    HistoryCommit::Replace {
                        state: "null".to_string(),
                    }
                } else {
                    HistoryCommit::Push {
                        state: "null".to_string(),
                    }
                };
                self.navigate_url(url, commit, redirect_depth + 1)
            }
            NavigationRequest::Submit { request } => self.navigate_request(
                request,
                HistoryCommit::Push {
                    state: "null".to_string(),
                },
                redirect_depth + 1,
            ),
            NavigationRequest::PushState { reference, state } => {
                self.apply_state_navigation(reference, state, false)?;
                Ok(NavigationStatus::Loaded)
            }
            NavigationRequest::ReplaceState { reference, state } => {
                self.apply_state_navigation(reference, state, true)?;
                Ok(NavigationStatus::Loaded)
            }
            NavigationRequest::Traverse { delta } => self.traverse_history(delta),
        }
    }

    fn apply_state_navigation(
        &mut self,
        reference: String,
        state: String,
        replace_history: bool,
    ) -> Result<(), NavigationError> {
        let base = self.current_url.clone().ok_or(NavigationError::NoPage)?;
        let url = base.resolve(&reference)?;
        let document_id = self.current_document_id.ok_or(NavigationError::NoPage)?;
        {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            page.runtime.set_location(url.clone());
            page.runtime
                .set_history_state_serialized(&state)
                .map_err(|error| NavigationError::Script(error.to_string()))?;
        }
        let commit = if replace_history {
            HistoryCommit::Replace { state }
        } else {
            HistoryCommit::Push { state }
        };
        self.commit_navigation(url, commit, document_id);
        Ok(())
    }

    fn traverse_history(&mut self, delta: isize) -> Result<NavigationStatus, NavigationError> {
        let current = self.history_index.ok_or(NavigationError::NoPage)?;
        let target = current as isize + delta;
        if target < 0 || target >= self.history.len() as isize {
            return Ok(NavigationStatus::Loaded);
        }
        let target = target as usize;
        let url = self.history[target].clone();
        let state = self.history_states[target].clone();
        let document_id = self.history_document_ids[target];
        if self.current_document_id == Some(document_id) {
            self.history_index = Some(target);
            self.current_url = Some(url.clone());
            {
                let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
                page.runtime.set_location(url);
                page.runtime
                    .set_history_position(self.history.len(), target);
                page.runtime
                    .set_history_state_serialized(&state)
                    .map_err(|error| NavigationError::Script(error.to_string()))?;
                page.runtime
                    .dispatch_popstate(&state)
                    .map_err(|error| NavigationError::Script(error.to_string()))?;
            }
            self.process_pending_navigation()?;
            return Ok(NavigationStatus::Loaded);
        }
        self.navigate_url(
            url,
            HistoryCommit::Traverse {
                index: target,
                state,
            },
            0,
        )
    }

    fn allocate_document_id(&mut self) -> u64 {
        let document_id = self.next_document_id;
        self.next_document_id = self.next_document_id.saturating_add(1);
        document_id
    }

    fn commit_new_document(&mut self, url: AppUrl, commit: HistoryCommit) {
        let document_id = self.allocate_document_id();
        self.commit_navigation(url, commit, document_id);
    }

    fn commit_navigation(&mut self, url: AppUrl, commit: HistoryCommit, document_id: u64) {
        match commit {
            HistoryCommit::Push { state } => {
                let next = self.history_index.map_or(0, |index| index + 1);
                self.history.truncate(next);
                self.history_states.truncate(next);
                self.history_document_ids.truncate(next);
                self.history.push(url.clone());
                self.history_states.push(state);
                self.history_document_ids.push(document_id);
                self.history_index = Some(next);
            }
            HistoryCommit::Replace { state } => {
                if let Some(index) = self.history_index {
                    self.history[index] = url.clone();
                    self.history_states[index] = state;
                    self.history_document_ids[index] = document_id;
                } else {
                    self.history.push(url.clone());
                    self.history_states.push(state);
                    self.history_document_ids.push(document_id);
                    self.history_index = Some(0);
                }
            }
            HistoryCommit::Traverse { index, state } => {
                self.history[index] = url.clone();
                self.history_states[index] = state;
                self.history_document_ids[index] = document_id;
                self.history_index = Some(index);
            }
        }
        self.current_url = Some(url.clone());
        self.current_document_id = Some(document_id);
        let history_length = self.history.len();
        let history_index = self.history_index.unwrap_or(0);
        if let Some(page) = self.page.as_mut() {
            page.runtime
                .set_history_position(history_length, history_index);
        }
    }

    fn load_stylesheets(
        &self,
        base: &AppUrl,
        sources: &[StylesheetSource],
    ) -> Result<String, NavigationError> {
        let mut stylesheet = String::new();
        for source in sources {
            let source = match source {
                StylesheetSource::Inline(source) => self.expand_imports(source.clone(), base, 0),
                StylesheetSource::External(reference) => {
                    let url = base.resolve(reference)?;
                    let resource = self.load_subresource(&url)?;
                    if !is_css(&resource) {
                        return Err(NavigationError::UnsupportedMimeType {
                            path: url.path().to_string(),
                            mime_type: resource.mime_type,
                        });
                    }
                    let text = String::from_utf8(resource.body)
                        .map_err(|_| NavigationError::InvalidUtf8(url.path().to_string()))?;
                    // urls in an external sheet are relative to it, not to the page
                    let text = crate::css::absolutize_urls(&text, &|reference| {
                        url.resolve(reference)
                            .ok()
                            .map(|absolute| absolute.to_string())
                    });
                    self.expand_imports(text, &url, 0)
                }
            };
            if !stylesheet.is_empty() {
                stylesheet.push('\n');
            }
            stylesheet.push_str(&source);
        }
        Ok(stylesheet)
    }

    // splices each @import's stylesheet in place of the rule, inside the layer, supports and
    // media conditions it names; one that fails to load is skipped, as a browser does
    fn expand_imports(&self, text: String, base: &AppUrl, depth: usize) -> String {
        const MAX_IMPORT_DEPTH: usize = 8;
        let (imports, statements, rest) = crate::css::leading_imports(&text);
        if imports.is_empty() {
            return text;
        }
        let mut expanded = statements;
        for import in imports {
            if depth >= MAX_IMPORT_DEPTH {
                continue;
            }
            let Ok(url) = base.resolve(&import.url) else {
                continue;
            };
            let Ok(resource) = self.load_subresource(&url) else {
                continue;
            };
            if !is_css(&resource) {
                continue;
            }
            let Ok(body) = String::from_utf8(resource.body) else {
                continue;
            };
            expanded.push_str(&import.open);
            expanded.push_str(&self.expand_imports(body, &url, depth + 1));
            expanded.push_str(&import.close);
            expanded.push('\n');
        }
        expanded.push_str(&rest);
        expanded
    }

    // fetches each @font-face rule's first source that loads and parses; a rule with none is
    // skipped, and its family falls back to installed faces
    fn load_web_fonts(&self, base: &AppUrl, stylesheet: &Stylesheet) {
        self.fonts.clear_web_faces();
        for rule in stylesheet.font_faces() {
            let face = rule.sources.iter().find_map(|source| {
                let url = base.resolve(source).ok()?;
                let resource = self.load_subresource(&url).ok()?;
                crate::font::FontFace::parse(resource.body).ok()
            });
            if let Some(face) = face {
                self.fonts.add_web_face(
                    &rule.family,
                    rule.weights,
                    rule.italic,
                    rule.ranges.clone(),
                    Rc::new(face),
                );
            }
        }
    }

    fn execute_scripts(
        &self,
        base: &AppUrl,
        sources: &[ScriptSource],
        runtime: &mut ScriptRuntime,
    ) -> Result<(), NavigationError> {
        let mut module_loader = PackagedModuleLoader {
            resources: &self.resources,
            provider: self.resource_provider.as_deref(),
        };
        for source in sources {
            let source = match source {
                ScriptSource::Inline(source) => source.clone(),
                ScriptSource::External(reference) => {
                    let url = base.resolve(reference)?;
                    let resource = self.load_subresource(&url)?;
                    if !is_javascript(&resource) {
                        return Err(NavigationError::UnsupportedMimeType {
                            path: url.path().to_string(),
                            mime_type: resource.mime_type,
                        });
                    }
                    String::from_utf8(resource.body)
                        .map_err(|_| NavigationError::InvalidUtf8(url.path().to_string()))?
                }
                ScriptSource::ImportMap(source) => {
                    runtime.add_import_map(source, base);
                    continue;
                }
                ScriptSource::InlineModule(source) => {
                    runtime
                        .execute_module(source, base.clone(), &mut module_loader)
                        .map_err(|error| NavigationError::Script(error.to_string()))?;
                    continue;
                }
                ScriptSource::ExternalModule(reference) => {
                    let url = base.resolve(reference)?;
                    let LoadedResource {
                        url: loaded_url,
                        resource,
                    } = self.load_subresource_with_url(&url)?;
                    if !is_javascript(&resource) {
                        return Err(NavigationError::UnsupportedMimeType {
                            path: loaded_url.path().to_string(),
                            mime_type: resource.mime_type,
                        });
                    }
                    let source = String::from_utf8(resource.body)
                        .map_err(|_| NavigationError::InvalidUtf8(loaded_url.path().to_string()))?;
                    runtime
                        .execute_module(&source, loaded_url, &mut module_loader)
                        .map_err(|error| NavigationError::Script(error.to_string()))?;
                    continue;
                }
            };
            runtime
                .execute(&source)
                .map_err(|error| NavigationError::Script(error.to_string()))?;
        }
        Ok(())
    }

    fn install_error_page(&mut self, url: &AppUrl, status: u16, detail: &str) {
        self.last_navigation_failure = Some(NavigationFailure {
            url: url.clone(),
            status,
            detail: detail.to_string(),
        });
        let source = error_page_source(status, &url.to_string(), detail);
        let document = Rc::new(RefCell::new(
            Document::parse(&source).expect("built-in recovery document should parse"),
        ));
        let stylesheet = Rc::new(Stylesheet::parse_recovering(&document.borrow().style_text()).0);
        let layout_inputs = self.layout_inputs(stylesheet.clone());
        self.page = Some(Page {
            runtime: {
                let mut runtime = ScriptRuntime::with_storage_and_runtime_info(
                    document.clone(),
                    self.local_storage.clone(),
                    self.session_storage.clone(),
                    self.host_runtime_info.clone(),
                );
                runtime.set_file_picker(self.file_picker.clone());
                runtime.set_clipboard_provider(self.clipboard_provider.clone());
                if let Some(time_zone) = &self.time_zone {
                    runtime.set_time_zone(time_zone.clone());
                }
                if let Some(locale) = &self.locale {
                    runtime.set_locale(locale.clone());
                }
                runtime.set_viewport_metrics(self.viewport);
                runtime.set_layout_inputs(layout_inputs.clone());
                runtime
            },
            document,
            stylesheet,
            layout_inputs,
            animated: false,
            animation_wake: Cell::new(None),
            images: RefCell::default(),
        });
    }
}

struct PackagedModuleLoader<'a> {
    resources: &'a ResourceBundle,
    provider: Option<&'a dyn ResourceProvider>,
}

impl ModuleSourceLoader for PackagedModuleLoader<'_> {
    fn load_module(
        &mut self,
        referrer: &AppUrl,
        reference: &str,
    ) -> Result<(AppUrl, String), crate::script::ScriptError> {
        let url = referrer
            .resolve(reference)
            .map_err(|error| runtime_error(error.to_string()))?;
        let (url, resource) = match self.resources.load_subresource(&url) {
            Ok(resource) => (url.clone(), resource),
            Err(NavigationError::ResourceNotFound(path)) => {
                let Some(provider) = self.provider else {
                    return Err(runtime_error(format!("module {path} was not found")));
                };
                let Some((loaded_url, resource)) = load_provider_resource(provider, &url)
                    .map_err(|error| runtime_error(error.to_string()))?
                else {
                    return Err(runtime_error(format!("module {path} was not found")));
                };
                (loaded_url, resource)
            }
            Err(error) => return Err(runtime_error(error.to_string())),
        };
        if !is_javascript(&resource) {
            return Err(runtime_error(format!(
                "module {} has unsupported MIME type {}",
                url.path(),
                resource.mime_type
            )));
        }
        let source = String::from_utf8(resource.body)
            .map_err(|_| runtime_error(format!("module {} is not UTF-8", url.path())))?;
        Ok((url, source))
    }
}

fn is_html(resource: &Resource) -> bool {
    resource
        .mime_type
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html"))
}

fn is_css(resource: &Resource) -> bool {
    resource
        .mime_type
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/css"))
}

pub(crate) fn is_javascript(resource: &Resource) -> bool {
    resource.mime_type.split(';').next().is_some_and(|mime| {
        matches!(
            mime.trim().to_ascii_lowercase().as_str(),
            "text/javascript"
                | "application/javascript"
                | "application/ecmascript"
                | "text/ecmascript"
        )
    })
}

pub(crate) fn is_json(resource: &Resource) -> bool {
    resource
        .mime_type
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
}

// elements are candidates in document order, which keeps sampling deterministic
// elements that a transition or animation rule matches, found in one walk that carries
// the ancestor chain instead of rebuilding it per element
fn collect_motion_candidates(
    stylesheet: &Stylesheet,
    state: &crate::css::StyleState,
    document: &Document,
) -> Vec<NodeId> {
    document
        .descendants(document.root())
        .filter(|&node| stylesheet.may_animate(document, node, state))
        .collect()
}

// image elements and boxes with background or mask images, in tree order
fn collect_image_sources(
    layout: &crate::layout::LayoutBox,
    sources: &mut Vec<(NodeId, Option<crate::image::ImageSource>)>,
) {
    for layer in layout.backgrounds.iter().chain(&layout.masks) {
        if let crate::css::LayerImage::Url(source) = layer.image {
            sources.push((layout.node, Some(source)));
        }
    }
    if layout.tag == "img" {
        sources.push((layout.node, None));
    }
    for child in &layout.children {
        collect_image_sources(child, sources);
    }
}

fn error_page_source(status: u16, url: &str, detail: &str) -> String {
    format!(
        r#"<main id="recovery" data-status="{status}" style="margin: 32px; padding: 24px; color: #d7e3ec; background-color: #122332; border: 2px solid #e06c75"><h1>ceri recovery {status}</h1><p>{}</p><p>{}</p></main>"#,
        escape_html(url),
        escape_html(detail)
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn normalize_resource_path(path: &str) -> Result<String, NavigationError> {
    if path.contains(['?', '#']) {
        return Err(NavigationError::InvalidResourcePath(path.to_string()));
    }
    let path = decode_path(path)
        .map_err(|error| NavigationError::InvalidResourcePath(format!("{path}: {error}")))?;
    Ok(if path == "/" {
        "/index.html".to_string()
    } else {
        path
    })
}

fn decode_path(raw_path: &str) -> Result<String, NavigationError> {
    let path = decode_path_bytes(raw_path)?;
    validate_path(&path)?;
    Ok(path)
}

fn resolve_reference_path(raw_path: &str) -> Result<String, NavigationError> {
    let path = decode_path_bytes(raw_path)?;
    if path == "/" {
        return Ok(path);
    }
    let mut segments = Vec::new();
    for segment in path
        .strip_prefix('/')
        .ok_or_else(|| NavigationError::InvalidUrl(path.clone()))?
        .split('/')
    {
        match segment {
            "" => return Err(NavigationError::EmptySegment),
            "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(NavigationError::DotSegment);
                }
            }
            segment => segments.push(segment),
        }
    }
    let mut normalized = format!("/{}", segments.join("/"));
    if path.ends_with('/') && normalized != "/" {
        normalized.push('/');
    }
    validate_path(&normalized)?;
    Ok(normalized)
}

fn decode_path_bytes(raw_path: &str) -> Result<String, NavigationError> {
    if !raw_path.starts_with('/') {
        return Err(NavigationError::InvalidUrl(raw_path.to_string()));
    }
    let bytes = raw_path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if byte == b'%' {
            let high = bytes
                .get(cursor + 1)
                .and_then(|value| hex_value(*value))
                .ok_or(NavigationError::InvalidPercentEncoding)?;
            let low = bytes
                .get(cursor + 2)
                .and_then(|value| hex_value(*value))
                .ok_or(NavigationError::InvalidPercentEncoding)?;
            let value = (high << 4) | low;
            reject_path_byte(value, true)?;
            decoded.push(value);
            cursor += 3;
            continue;
        }
        reject_path_byte(byte, false)?;
        decoded.push(byte);
        cursor += 1;
    }
    String::from_utf8(decoded).map_err(|_| NavigationError::UnsafeCharacter)
}

fn reject_path_byte(byte: u8, encoded: bool) -> Result<(), NavigationError> {
    match byte {
        b'/' if encoded => Err(NavigationError::EncodedSeparator),
        b'\\' => Err(if encoded {
            NavigationError::EncodedSeparator
        } else {
            NavigationError::Backslash
        }),
        0 => Err(NavigationError::NulByte),
        b':' => Err(NavigationError::Colon),
        1..=31 | 127 => Err(NavigationError::UnsafeCharacter),
        b'<' | b'>' | b'"' | b'|' | b'?' | b'*' => Err(NavigationError::UnsafeCharacter),
        _ => Ok(()),
    }
}

fn validate_path(path: &str) -> Result<(), NavigationError> {
    if path == "/" {
        return Ok(());
    }
    for segment in path
        .strip_prefix('/')
        .ok_or_else(|| NavigationError::InvalidUrl(path.to_string()))?
        .split('/')
    {
        if segment.is_empty() {
            return Err(NavigationError::EmptySegment);
        }
        if matches!(segment, "." | "..") {
            return Err(NavigationError::DotSegment);
        }
        if segment.ends_with('.') || segment.ends_with(' ') {
            return Err(NavigationError::UnsafeWindowsName);
        }
        if is_windows_device_name(segment) {
            return Err(NavigationError::UnsafeWindowsName);
        }
    }
    Ok(())
}

fn is_extensionless_path(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_none_or(|name| !name.contains('.'))
}

fn is_windows_device_name(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or(segment);
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || matches!(
            upper.strip_prefix("COM"),
            Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        )
        || matches!(
            upper.strip_prefix("LPT"),
            Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        )
}

// a packaged app is bounded, so a mistaken root cannot pull an entire disk into memory
const MAX_BUNDLE_FILES: usize = 20_000;
const MAX_BUNDLE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Default)]
struct BundleBudget {
    files: usize,
    bytes: u64,
}

impl BundleBudget {
    fn take(&mut self, bytes: u64) -> io::Result<()> {
        self.files += 1;
        self.bytes += bytes;
        if self.files > MAX_BUNDLE_FILES {
            return Err(io::Error::other(format!(
                "resource root holds more than {MAX_BUNDLE_FILES} files"
            )));
        }
        if self.bytes > MAX_BUNDLE_BYTES {
            return Err(io::Error::other(format!(
                "resource root holds more than {} MiB",
                MAX_BUNDLE_BYTES / (1024 * 1024)
            )));
        }
        Ok(())
    }
}

fn collect_directory(
    root: &Path,
    directory: &Path,
    resources: &mut ResourceBundle,
    visited: &mut BTreeSet<PathBuf>,
    budget: &mut BundleBudget,
) -> io::Result<()> {
    let canonical_directory = fs::canonicalize(directory)?;
    if !canonical_directory.starts_with(root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "resource directory escapes the bundle root",
        ));
    }
    if !visited.insert(canonical_directory) {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir()
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| matches!(name, ".git" | "target"))
        {
            continue;
        }
        let canonical_path = fs::canonicalize(&path)?;
        if !canonical_path.starts_with(root) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "resource path escapes the bundle root",
            ));
        }
        if canonical_path.is_dir() {
            collect_directory(root, &path, resources, visited, budget)?;
            continue;
        }
        if !canonical_path.is_file() {
            continue;
        }
        let relative = path.strip_prefix(root).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "resource path is not relative to the bundle root",
            )
        })?;
        let url_path = relative_url_path(relative)?;
        budget.take(entry.metadata()?.len())?;
        let body = fs::read(&path)?;
        resources
            .insert(&url_path, mime_type_for(&path), body)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    }
    Ok(())
}

fn relative_url_path(path: &Path) -> io::Result<String> {
    let mut segments = Vec::new();
    for component in path.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "resource path contains a non-normal component",
            ));
        };
        let segment = component.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "resource path is not valid UTF-8",
            )
        })?;
        segments.push(segment);
    }
    if segments.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "resource path is empty",
        ));
    }
    Ok(format!("/{}", segments.join("/")))
}

pub fn mime_type_for(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "css" => "text/css",
        "htm" | "html" => "text/html",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "wasm" => "application/wasm",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

pub(crate) fn normalize_headers(
    headers: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, NavigationError> {
    let mut normalized = BTreeMap::new();
    for (name, value) in headers {
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() || !name.bytes().all(is_http_token_byte) {
            return Err(NavigationError::InvalidRequestHeader(name));
        }
        let value = value.trim().to_string();
        if value
            .bytes()
            .any(|byte| matches!(byte, b'\r' | b'\n' | 0..=31 | 127))
        {
            return Err(NavigationError::InvalidRequestHeader(name));
        }
        normalized.insert(name, value);
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::{Color, Cursor};
    use std::cell::RefCell;
    use std::rc::Rc;

    struct MemoryProvider {
        resources: BTreeMap<String, Resource>,
    }

    impl ResourceProvider for MemoryProvider {
        fn provide(&self, url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
            Ok(self.resources.get(url.path()).cloned())
        }
    }

    struct FailingProvider;

    impl ResourceProvider for FailingProvider {
        fn provide(&self, _url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
            Err(NavigationError::ResourceProvider(
                "transport unavailable".to_string(),
            ))
        }
    }

    struct StatusProvider {
        status: u16,
        paths: Rc<RefCell<Vec<String>>>,
    }

    impl ResourceProvider for StatusProvider {
        fn provide(&self, _url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
            Ok(None)
        }

        fn provide_response(
            &self,
            request: &ResourceRequest,
        ) -> Result<Option<ResourceResponse>, NavigationError> {
            self.paths
                .borrow_mut()
                .push(request.url().path().to_string());
            ResourceResponse::new(
                self.status,
                "text/html",
                BTreeMap::new(),
                b"upstream status".to_vec(),
            )
            .map(Some)
        }
    }

    struct RedirectProvider {
        location: String,
        paths: Rc<RefCell<Vec<String>>>,
    }

    impl ResourceProvider for RedirectProvider {
        fn provide(&self, _url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
            Ok(None)
        }

        fn provide_response(
            &self,
            request: &ResourceRequest,
        ) -> Result<Option<ResourceResponse>, NavigationError> {
            let path = request.url().path().to_string();
            self.paths.borrow_mut().push(path.clone());
            match path.as_str() {
                "/" => {
                    let mut headers = BTreeMap::new();
                    headers.insert("Location".to_string(), self.location.clone());
                    ResourceResponse::new(302, "text/html", headers, Vec::new()).map(Some)
                }
                "/login" => ResourceResponse::new(
                    200,
                    "text/html",
                    BTreeMap::new(),
                    b"<p id='status'>login</p>".to_vec(),
                )
                .map(Some),
                _ => Ok(None),
            }
        }
    }

    struct ModuleRedirectProvider;

    impl ResourceProvider for ModuleRedirectProvider {
        fn provide(&self, _url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
            Ok(None)
        }

        fn provide_response(
            &self,
            request: &ResourceRequest,
        ) -> Result<Option<ResourceResponse>, NavigationError> {
            let response = match request.url().path() {
                "/" => ResourceResponse::new(
                    200,
                    "text/html",
                    BTreeMap::new(),
                    br#"<p id="status">idle</p><script type="module" src="/entry.js"></script>"#
                        .to_vec(),
                ),
                "/entry.js" => {
                    let mut headers = BTreeMap::new();
                    headers.insert("Location".to_string(), "/modules/entry.js".to_string());
                    ResourceResponse::new(302, "text/javascript", headers, Vec::new())
                }
                "/modules/entry.js" => ResourceResponse::new(
                    200,
                    "text/javascript",
                    BTreeMap::new(),
                    br#"import { message } from "./message.js"; document.getElementById("status").textContent = message;"#
                        .to_vec(),
                ),
                "/modules/message.js" => ResourceResponse::new(
                    200,
                    "text/javascript",
                    BTreeMap::new(),
                    br#"export const message = "redirected";"#.to_vec(),
                ),
                _ => return Ok(None),
            }?;
            Ok(Some(response))
        }
    }

    struct ShellProvider;

    impl ResourceProvider for ShellProvider {
        fn provide(&self, url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
            Ok((url.path() == "/index.html")
                .then(|| Resource::new("text/html", b"<p id='status'>shell</p>".to_vec())))
        }
    }

    struct RequestProvider {
        resources: BTreeMap<String, Resource>,
        requests: Rc<RefCell<Vec<ResourceRequest>>>,
    }

    impl ResourceProvider for RequestProvider {
        fn provide(&self, url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
            Ok(self.resources.get(url.path()).cloned())
        }

        fn provide_response(
            &self,
            request: &ResourceRequest,
        ) -> Result<Option<ResourceResponse>, NavigationError> {
            self.requests.borrow_mut().push(request.clone());
            let Some(resource) = self.provide(request.url())? else {
                return Ok(None);
            };
            let mut headers = BTreeMap::new();
            headers.insert("X-Provider".to_string(), "yes".to_string());
            ResourceResponse::new(
                201,
                resource.mime_type().to_string(),
                headers,
                resource.body().to_vec(),
            )
            .map(Some)
        }
    }

    struct FormProvider {
        requests: Rc<RefCell<Vec<ResourceRequest>>>,
        multipart: bool,
    }

    struct UploadProvider {
        requests: Rc<RefCell<Vec<ResourceRequest>>>,
    }

    impl ResourceProvider for UploadProvider {
        fn provide(&self, _url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
            Ok(None)
        }

        fn provide_response(
            &self,
            request: &ResourceRequest,
        ) -> Result<Option<ResourceResponse>, NavigationError> {
            self.requests.borrow_mut().push(request.clone());
            match (request.method(), request.url().path()) {
                ("GET", "/") => ResourceResponse::new(
                    200,
                    "text/html",
                    BTreeMap::new(),
                    br#"<form action="/upload" method="post" enctype="multipart/form-data"><input id="file" name="upload" type="file" accept="text/plain"><button id="submit" type="submit">send</button></form><p id="status">idle</p><script>const input = document.getElementById("file"); const status = document.getElementById("status"); input.addEventListener("change", () => { const file = input.files.item(0); status.textContent = input.files.length + "|" + file.name + "|" + file.type + "|" + file.lastModified; });</script>"#.to_vec(),
                )
                .map(Some),
                ("POST", "/upload") => ResourceResponse::new(
                    200,
                    "text/html",
                    BTreeMap::new(),
                    br#"<p id="status">uploaded</p>"#.to_vec(),
                )
                .map(Some),
                _ => Ok(None),
            }
        }
    }

    struct StaticFilePicker {
        requests: Rc<RefCell<Vec<FilePickerRequest>>>,
        selection: Option<Vec<FileSelection>>,
    }

    impl FilePicker for StaticFilePicker {
        fn pick_files(
            &self,
            request: &FilePickerRequest,
        ) -> Result<Option<Vec<FileSelection>>, NavigationError> {
            self.requests.borrow_mut().push(request.clone());
            Ok(self.selection.clone())
        }
    }

    struct MemoryClipboard {
        text: Rc<RefCell<String>>,
        writes: Rc<RefCell<Vec<String>>>,
    }

    impl ClipboardProvider for MemoryClipboard {
        fn read_text(&self) -> Result<String, NavigationError> {
            Ok(self.text.borrow().clone())
        }

        fn write_text(&self, text: &str) -> Result<(), NavigationError> {
            self.writes.borrow_mut().push(text.to_string());
            *self.text.borrow_mut() = text.to_string();
            Ok(())
        }
    }

    struct DeniedClipboard;

    impl ClipboardProvider for DeniedClipboard {
        fn read_text(&self) -> Result<String, NavigationError> {
            Err(NavigationError::Clipboard("permission denied".to_string()))
        }

        fn write_text(&self, _text: &str) -> Result<(), NavigationError> {
            Err(NavigationError::Clipboard("permission denied".to_string()))
        }
    }

    impl ResourceProvider for FormProvider {
        fn provide(&self, _url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
            Ok(None)
        }

        fn provide_response(
            &self,
            request: &ResourceRequest,
        ) -> Result<Option<ResourceResponse>, NavigationError> {
            self.requests.borrow_mut().push(request.clone());
            match (request.method(), request.url().path()) {
                ("GET", "/") => {
                    let enctype = if self.multipart {
                        " enctype=\"multipart/form-data\""
                    } else {
                        ""
                    };
                    ResourceResponse::new(
                        200,
                        "text/html",
                        BTreeMap::new(),
                        format!(
                            "<form action=\"/submit\" method=\"post\"{enctype}><input name=\"q\" value=\"rust browser\"><button id=\"submit\" type=\"submit\">send</button></form>"
                        )
                        .into_bytes(),
                    )
                    .map(Some)
                }
                ("POST", "/submit") => {
                    if self.multipart {
                        return ResourceResponse::new(
                            200,
                            "text/html",
                            BTreeMap::new(),
                            br#"<p id="status">multipart posted</p>"#.to_vec(),
                        )
                        .map(Some);
                    }
                    let mut headers = BTreeMap::new();
                    headers.insert("Location".to_string(), "/result.html".to_string());
                    ResourceResponse::new(303, "text/html", headers, Vec::new()).map(Some)
                }
                ("GET", "/result.html") => ResourceResponse::new(
                    200,
                    "text/html",
                    BTreeMap::new(),
                    br#"<p id="status">posted</p>"#.to_vec(),
                )
                .map(Some),
                _ => Ok(None),
            }
        }
    }

    // the pages these tests build lay out from the viewport's corner, as most apps reset it
    const PAGE_RESET: &str = "<style>body { margin: 0 }</style>";

    fn bundle(shell: &str) -> ResourceBundle {
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/index.html",
                "text/html; charset=utf-8",
                format!("{PAGE_RESET}{shell}").into_bytes(),
            )
            .expect("shell should be accepted");
        resources
    }

    #[test]
    fn loads_a_packaged_directory_without_build_outputs() {
        let resources =
            ResourceBundle::from_directory(env!("CARGO_MANIFEST_DIR")).expect("root should load");
        assert_eq!(
            resources
                .resource("/demo.css")
                .expect("demo stylesheet should be packaged")
                .mime_type(),
            "text/css"
        );
        assert_eq!(
            resources
                .resource("/demo.js")
                .expect("demo script should be packaged")
                .mime_type(),
            "text/javascript"
        );
        assert!(
            resources
                .resource("/target/debug/ceri-engine.exe")
                .is_none()
        );
    }

    #[test]
    fn browser_pumps_a_packaged_worker_without_a_loopback_server() {
        let mut resources = bundle(
            r#"<p id='status'>idle</p><script>
              const status = document.getElementById("status");
              const worker = new Worker("/worker.js");
              worker.onmessage = (event) => { status.textContent = event.data; };
              worker.postMessage("ready");
            </script>"#,
        );
        resources
            .insert(
                "/worker.js",
                "text/javascript",
                br#"self.onmessage = (event) => { postMessage(event.data + "!"); };"#.to_vec(),
            )
            .expect("worker resource should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/index.html")
            .expect("packaged document should load");
        for _ in 0..100 {
            if browser.pump().expect("worker pump should execute") > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("ready!")
        );
    }

    #[test]
    fn browser_pumps_a_provider_worker_without_a_loopback_server() {
        let mut provider_resources = BTreeMap::new();
        provider_resources.insert(
            "/index.html".to_string(),
            Resource::new(
                "text/html",
                br#"<p id='status'>idle</p><script>
                  const status = document.getElementById("status");
                  const worker = new Worker("/worker.js");
                  worker.onmessage = (event) => { status.textContent = event.data; };
                  worker.postMessage("provider");
                </script>"#
                    .to_vec(),
            ),
        );
        provider_resources.insert(
            "/worker.js".to_string(),
            Resource::new(
                "text/javascript",
                br#"self.onmessage = (event) => { postMessage(event.data + "!"); };"#.to_vec(),
            ),
        );
        let mut browser = Browser::with_resource_provider(
            ResourceBundle::default(),
            Rc::new(MemoryProvider {
                resources: provider_resources,
            }),
        );
        browser
            .navigate("https://app.ceri.invalid/index.html")
            .expect("provider document should load");
        for _ in 0..100 {
            if browser.pump().expect("worker pump should execute") > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("provider!")
        );
    }

    #[test]
    fn provider_worker_loads_module_imports_and_fetches_subresources() {
        let mut provider_resources = BTreeMap::new();
        provider_resources.insert(
            "/index.html".to_string(),
            Resource::new(
                "text/html",
                br#"<p id='status'>idle</p><script>
                  const status = document.getElementById("status");
                  const worker = new Worker("/worker.js", { type: "module" });
                  worker.addEventListener("message", (event) => { status.textContent = event.data; });
                  worker.addEventListener("error", (event) => { status.textContent = "error:" + event.message; });
                  worker.postMessage("provider");
                </script>"#
                .to_vec(),
            ),
        );
        provider_resources.insert(
            "/worker.js".to_string(),
            Resource::new(
                "text/javascript",
                br#"import { suffix } from "./worker-child.js";
                self.addEventListener("message", (event) => {
                  fetch("./worker-data.txt")
                    .then((response) => response.text())
                    .then((body) => postMessage(suffix(event.data) + ":" + body));
                });"#
                    .to_vec(),
            ),
        );
        provider_resources.insert(
            "/worker-child.js".to_string(),
            Resource::new(
                "text/javascript",
                b"export function suffix(value) { return value + '!'; }".to_vec(),
            ),
        );
        provider_resources.insert(
            "/worker-data.txt".to_string(),
            Resource::new("text/plain", b"remote".to_vec()),
        );
        let mut browser = Browser::with_resource_provider(
            ResourceBundle::default(),
            Rc::new(MemoryProvider {
                resources: provider_resources,
            }),
        );
        browser
            .navigate("https://app.ceri.invalid/index.html")
            .expect("provider document should load");
        for _ in 0..200 {
            if browser.pump().expect("worker pump should execute") > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("provider!:remote")
        );
    }

    #[test]
    fn classic_provider_worker_imports_scripts_without_a_loopback_server() {
        let mut provider_resources = BTreeMap::new();
        provider_resources.insert(
            "/index.html".to_string(),
            Resource::new(
                "text/html",
                br#"<p id='status'>idle</p><script>
                  const status = document.getElementById("status");
                  const worker = new Worker("/worker.js");
                  worker.onmessage = (event) => { status.textContent = event.data; };
                  worker.postMessage("classic");
                </script>"#
                    .to_vec(),
            ),
        );
        provider_resources.insert(
            "/worker.js".to_string(),
            Resource::new(
                "text/javascript",
                b"importScripts('./worker-helper.js');".to_vec(),
            ),
        );
        provider_resources.insert(
            "/worker-helper.js".to_string(),
            Resource::new(
                "text/javascript",
                b"self.onmessage = (event) => { postMessage(event.data + '!'); };".to_vec(),
            ),
        );
        let mut browser = Browser::with_resource_provider(
            ResourceBundle::default(),
            Rc::new(MemoryProvider {
                resources: provider_resources,
            }),
        );
        browser
            .navigate("https://app.ceri.invalid/index.html")
            .expect("provider document should load");
        for _ in 0..200 {
            if browser.pump().expect("worker pump should execute") > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("classic!")
        );
    }

    #[test]
    fn browser_instantiates_provider_wasm_without_a_loopback_server() {
        let wasm = vec![
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x07, 0x01, 0x60, 0x02, 0x7f,
            0x7f, 0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, 0x61, 0x64, 0x64,
            0x00, 0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b,
        ];
        let mut provider_resources = BTreeMap::new();
        provider_resources.insert(
            "/index.html".to_string(),
            Resource::new(
                "text/html",
                br#"<p id='status'>idle</p><script>
                  const status = document.getElementById("status");
                  fetch("/add.wasm")
                    .then((response) => response.arrayBuffer())
                    .then((bytes) => WebAssembly.instantiate(bytes))
                    .then((result) => { status.textContent = result.instance.exports.add(2, 3); });
                </script>"#
                    .to_vec(),
            ),
        );
        provider_resources.insert(
            "/add.wasm".to_string(),
            Resource::new("application/wasm", wasm),
        );
        let mut browser = Browser::with_resource_provider(
            ResourceBundle::default(),
            Rc::new(MemoryProvider {
                resources: provider_resources,
            }),
        );
        browser
            .navigate("https://app.ceri.invalid/index.html")
            .expect("provider document should load");
        for _ in 0..300 {
            browser.pump().expect("Wasm page task should execute");
            let ready = browser.document().ok().and_then(|document| {
                let document = document.borrow();
                let status = document.find_element_by_id("status")?;
                (document.element_text_content(status).as_deref() == Some("5")).then_some(())
            });
            if ready.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("5")
        );
    }

    #[test]
    fn parses_app_urls_without_a_loopback_authority() {
        let url = AppUrl::parse("HTTPS://APP.CERI.INVALID/docs/intro%2Ehtml?q=rust#top")
            .expect("app URL should parse");
        assert_eq!(url.path(), "/docs/intro.html");
        assert_eq!(url.query(), "q=rust");
        assert_eq!(url.fragment(), "top");
        assert_eq!(
            url.to_string(),
            "https://app.ceri.invalid/docs/intro.html?q=rust#top"
        );
    }

    #[test]
    fn external_requests_leave_the_app_origin_only_when_the_host_allows_them() {
        use std::collections::BTreeMap;

        struct SingleOrigin;

        impl ResourceProvider for SingleOrigin {
            fn provide(&self, _url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
                Ok(None)
            }

            fn provide_external(
                &self,
                request: &ExternalRequest,
            ) -> Result<Option<ResourceResponse>, NavigationError> {
                if request.origin() != "https://api.example.test" {
                    return Ok(None);
                }
                ResourceResponse::new(
                    200,
                    "application/json",
                    BTreeMap::new(),
                    br#"{"ok":true}"#.to_vec(),
                )
                .map(Some)
            }
        }

        assert_eq!(
            external_url("https://api.example.test/v1/items?page=2")
                .expect("an https target should be accepted"),
            "https://api.example.test/v1/items?page=2"
        );
        assert!(matches!(
            external_url("http://api.example.test"),
            Err(NavigationError::InsecureScheme)
        ));
        assert!(matches!(
            external_url("https://app.ceri.invalid/data"),
            Err(NavigationError::WrongOrigin)
        ));
        assert!(external_url("https://user:pass@api.example.test").is_err());
        assert!(external_url("https://api.example.test/#fragment").is_err());

        let request = ExternalRequest::new(
            "https://api.example.test/v1/items",
            "post",
            BTreeMap::from([("content-type".to_string(), "application/json".to_string())]),
            b"{}".to_vec(),
        )
        .expect("a post request should build");
        assert_eq!(request.method(), "POST");
        assert_eq!(request.origin(), "https://api.example.test");
        let response = SingleOrigin
            .provide_external(&request)
            .expect("the allowed origin should answer")
            .expect("a response should exist");
        assert_eq!(response.status(), 200);

        let refused = ExternalRequest::new(
            "https://other.example.test/v1",
            "GET",
            BTreeMap::new(),
            Vec::new(),
        )
        .expect("a get request should build");
        assert!(
            SingleOrigin
                .provide_external(&refused)
                .expect("a refusal is not an error")
                .is_none()
        );

        // the default provider refuses every outside origin
        struct PackagedOnly;
        impl ResourceProvider for PackagedOnly {
            fn provide(&self, _url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
                Ok(None)
            }
        }
        assert!(
            PackagedOnly
                .provide_external(&request)
                .expect("the default refusal is not an error")
                .is_none()
        );
    }

    #[test]
    fn resolves_same_origin_resource_references() {
        let base = AppUrl::parse("https://app.ceri.invalid/docs/index.html?q=rust#top")
            .expect("base URL should parse");
        assert_eq!(
            base.resolve("theme.css")
                .expect("relative URL should resolve")
                .path(),
            "/docs/theme.css"
        );
        assert_eq!(
            base.resolve("../theme.css")
                .expect("parent URL should resolve")
                .path(),
            "/theme.css"
        );
        assert_eq!(
            base.resolve("/assets/app.js")
                .expect("root URL should resolve")
                .path(),
            "/assets/app.js"
        );
        assert_eq!(
            base.resolve("/")
                .expect("root navigation should resolve")
                .path(),
            "/"
        );
        let query = base.resolve("?version=2").expect("query should resolve");
        assert_eq!(query.path(), "/docs/index.html");
        assert_eq!(query.query(), "version=2");
        assert_eq!(query.fragment(), "");
        let fragment = base.resolve("#section").expect("fragment should resolve");
        assert_eq!(fragment.path(), "/docs/index.html");
        assert_eq!(fragment.query(), "q=rust");
        assert_eq!(fragment.fragment(), "section");
        assert_eq!(
            base.resolve("https://other.invalid/app.js")
                .expect_err("cross-origin URL should fail"),
            NavigationError::WrongOrigin
        );
    }

    #[test]
    fn rejects_cross_origin_and_unsafe_paths() {
        for (input, expected) in [
            ("http://app.ceri.invalid/", NavigationError::InsecureScheme),
            ("https://other.invalid/", NavigationError::WrongOrigin),
            (
                "https://app.ceri.invalid/%2e%2e/private",
                NavigationError::DotSegment,
            ),
            (
                "https://app.ceri.invalid/assets%2fapp.js",
                NavigationError::EncodedSeparator,
            ),
        ] {
            assert_eq!(
                AppUrl::parse(input).expect_err("URL should be rejected"),
                expected
            );
        }
    }

    #[test]
    fn navigates_between_packaged_html_resources() {
        let mut resources = bundle("<p id='status'>home</p>");
        resources
            .insert(
                "/about.html",
                "text/html",
                b"<p id='status'>about</p>".to_vec(),
            )
            .expect("about page should be accepted");
        let mut browser = Browser::new(resources);

        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("home should load"),
            NavigationStatus::Loaded
        );
        assert_eq!(browser.current_url().unwrap().path(), "/");
        assert_eq!(browser.history().len(), 1);
        assert_eq!(
            {
                let document = browser.document().unwrap();
                let document = document.borrow();
                document.element_text_content(document.node_at(&[0, 1]))
            }
            .as_deref(),
            Some("home")
        );

        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/about.html")
                .expect("about should load"),
            NavigationStatus::Loaded
        );
        assert_eq!(browser.current_url().unwrap().path(), "/about.html");
        assert_eq!(browser.history().len(), 2);
    }

    #[test]
    fn validates_resource_requests_before_the_provider_boundary() {
        let url = AppUrl::parse("https://app.ceri.invalid/api").expect("request URL should parse");
        let mut headers = BTreeMap::new();
        headers.insert("X-Trace".to_string(), "lab".to_string());
        let request = ResourceRequest::new(url, "post", headers, br#"{"ok":true}"#.to_vec())
            .expect("request should be accepted");
        assert_eq!(request.method(), "POST");
        assert_eq!(request.headers().get("x-trace"), Some(&"lab".to_string()));
        assert_eq!(request.body(), br#"{"ok":true}"#);
        assert_eq!(
            ResourceRequest::new(
                request.url().clone(),
                "GET",
                BTreeMap::new(),
                b"body".to_vec()
            )
            .expect_err("GET bodies should be rejected"),
            NavigationError::RequestBodyNotAllowed("GET".to_string())
        );
        let mut response_headers = BTreeMap::new();
        response_headers.insert("Content-Type".to_string(), "application/json".to_string());
        let response = ResourceResponse::new(
            201,
            "application/json; charset=utf-8",
            response_headers,
            br#"{"ok":true}"#.to_vec(),
        )
        .expect("response should be accepted");
        assert_eq!(response.status(), 201);
        assert_eq!(
            response.headers().get("content-type"),
            Some(&"application/json".to_string())
        );
        assert_eq!(
            ResourceResponse::new(99, "text/plain", BTreeMap::new(), Vec::new())
                .expect_err("invalid response statuses should be rejected"),
            NavigationError::InvalidResponseStatus(99)
        );
    }

    #[test]
    fn activates_nested_links_without_a_loopback_server() {
        let mut resources = bundle(
            r#"<main><a id="go" href="/about.html"><span id="label">open</span></a><p id="status">home</p></main>"#,
        );
        resources
            .insert(
                "/about.html",
                "text/html",
                br#"<p id="status">about</p>"#.to_vec(),
            )
            .expect("about page should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("home should load");
        assert!(
            browser
                .click_id("label")
                .expect("nested link should activate")
        );
        assert_eq!(browser.current_url().unwrap().path(), "/about.html");
        assert_eq!(browser.history().len(), 2);
        let document = browser.document().expect("about document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("about")
        );
    }

    #[test]
    fn label_click_activates_associated_control_without_a_loopback_server() {
        let mut browser = Browser::new(bundle(
            r#"<label id="name-label" for="name">Name</label><input id="name"><p id="status">idle</p><script>
                const input = document.getElementById("name");
                input.addEventListener("focus", () => {
                    document.getElementById("status").textContent = "focused:" + document.activeElement.id;
                });
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("label page should load");
        assert!(
            browser
                .click_id("name-label")
                .expect("label click should activate the input")
        );
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("focused:name")
        );
    }

    #[test]
    fn navigation_applies_autofocus_after_script_startup_without_a_loopback_server() {
        let mut browser = Browser::new(bundle(
            r#"<div autofocus>ignored</div><input id="name" autofocus><p id="status">idle</p><script>
                document.getElementById("name").addEventListener("focus", () => {
                    document.getElementById("status").textContent = "focused:" + document.activeElement.id;
                });
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("autofocus page should load");
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("focused:name")
        );
    }

    #[test]
    fn readonly_navigation_control_rejects_host_edits_without_a_loopback_server() {
        let mut browser = Browser::new(bundle(
            r#"<input id="name" value="rust" readonly><p id="status">idle</p><script>
                const input = document.getElementById("name");
                input.addEventListener("focus", () => { document.getElementById("status").textContent = "focused"; });
                input.addEventListener("input", () => { document.getElementById("status").textContent = "edited"; });
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("readonly page should load");
        assert!(
            browser
                .click_id("name")
                .expect("readonly control should focus")
        );
        assert!(
            !browser
                .insert_text("!")
                .expect("readonly control should reject host text")
        );
        assert!(
            !browser
                .key_down("Backspace")
                .expect("readonly control should reject deletion")
        );
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("focused")
        );
    }

    #[test]
    fn maxlength_navigation_control_caps_host_text_without_a_loopback_server() {
        let mut browser = Browser::new(bundle(
            r#"<input id="name" maxlength="4" value="ab"><p id="status">idle</p><script>
                const input = document.getElementById("name");
                input.addEventListener("input", (event) => {
                    document.getElementById("status").textContent = event.data + "|" + input.value;
                });
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("maxlength page should load");
        assert!(
            browser
                .click_id("name")
                .expect("maxlength control should focus")
        );
        assert!(
            browser
                .insert_text("😀x")
                .expect("host text should stop at maxlength")
        );
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("😀|ab😀")
        );
    }

    #[test]
    fn invalid_form_submission_stays_on_packaged_page_without_a_loopback_server() {
        let mut resources = bundle(
            r#"<form action="/done.html"><input id="name" required><button id="submit" type="submit">send</button></form><p id="status">idle</p><script>
                const form = document.querySelector("form");
                const name = document.getElementById("name");
                name.addEventListener("invalid", () => { document.getElementById("status").textContent = "invalid"; });
                form.addEventListener("submit", () => { document.getElementById("status").textContent = "submitted"; });
            </script>"#,
        );
        resources
            .insert(
                "/done.html",
                "text/html",
                br#"<p id="status">done</p>"#.to_vec(),
            )
            .expect("destination page should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("validation page should load");
        assert!(
            browser
                .click_id("submit")
                .expect("invalid submit click should dispatch")
        );
        assert_eq!(browser.current_url().unwrap().path(), "/");
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("invalid")
        );
        assert!(browser.click_id("name").expect("name input should focus"));
        assert!(
            browser
                .insert_text("Ceri")
                .expect("required value should accept host text")
        );
        assert!(
            browser
                .click_id("submit")
                .expect("valid submit click should navigate")
        );
        assert_eq!(browser.current_url().unwrap().path(), "/done.html");
    }

    #[test]
    fn click_default_navigation_respects_prevent_default() {
        let mut browser = Browser::new(bundle(
            r#"<a id="go" href="/about.html"><span id="label">open</span></a><p id="status">home</p><script>
                const link = document.getElementById("go");
                const status = document.getElementById("status");
                link.addEventListener("click", (event) => {
                    event.preventDefault();
                    status.textContent = event.defaultPrevented;
                });
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("home should load");
        assert!(browser.click_id("label").expect("click should dispatch"));
        assert_eq!(browser.current_url().unwrap().path(), "/");
        assert_eq!(browser.history().len(), 1);
        let document = browser.document().expect("home document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("true")
        );
    }

    #[test]
    fn location_replace_navigates_and_replaces_history() {
        let mut resources = bundle(
            r#"<button id="go">open</button><p id="status">home</p><script>
                document.getElementById("go").addEventListener("click", () => {
                    window.location.replace("/about.html");
                });
            </script>"#,
        );
        resources
            .insert(
                "/about.html",
                "text/html",
                br#"<p id="status">about</p>"#.to_vec(),
            )
            .expect("about page should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("home should load");
        assert!(
            browser
                .click_id("go")
                .expect("location replace should navigate")
        );
        assert_eq!(browser.current_url().unwrap().path(), "/about.html");
        assert_eq!(browser.history().len(), 1);
        assert_eq!(browser.history()[0].path(), "/about.html");
    }

    #[test]
    fn location_assignment_navigates_with_push_history() {
        let mut resources = bundle(
            r#"<button id="go">open</button><script>
                document.getElementById("go").addEventListener("click", () => {
                    window.location = "/about.html";
                });
            </script>"#,
        );
        resources
            .insert(
                "/about.html",
                "text/html",
                br#"<p id="status">about</p>"#.to_vec(),
            )
            .expect("about page should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("home should load");
        assert!(
            browser
                .click_id("go")
                .expect("location assignment should navigate")
        );
        assert_eq!(browser.current_url().unwrap().path(), "/about.html");
        assert_eq!(browser.history().len(), 2);
    }

    #[test]
    fn spa_history_push_state_and_popstate_keep_the_document() {
        let mut browser = Browser::new(bundle(
            r#"<button id="push">push</button><button id="back">back</button><p id="status">home</p><script>
                const status = document.getElementById("status");
                window.addEventListener("popstate", (event) => {
                    if (event.state === null) {
                        status.textContent = location.pathname + "|none";
                    } else {
                        status.textContent = location.pathname + "|" + event.state.view;
                    }
                });
                document.getElementById("push").addEventListener("click", () => {
                    window.history.pushState({ view: "details" }, "", "/details");
                    status.textContent = location.pathname + "|" + window.history.state.view + "/" + window.history.length;
                });
                document.getElementById("back").addEventListener("click", () => {
                    window.history.back();
                });
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("home should load");
        let home_document = browser.document().expect("home document should exist");
        assert!(browser.click_id("push").expect("pushState should run"));
        assert_eq!(browser.current_url().unwrap().path(), "/details");
        assert_eq!(browser.history().len(), 2);
        assert!(Rc::ptr_eq(
            &home_document,
            &browser
                .document()
                .expect("SPA document should remain loaded")
        ));
        let status = browser
            .document()
            .expect("SPA document should exist")
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            browser
                .document()
                .expect("SPA document should exist")
                .borrow()
                .element_text_content(status)
                .as_deref(),
            Some("/details|details/2")
        );
        assert!(browser.click_id("back").expect("history.back should run"));
        assert_eq!(browser.current_url().unwrap().path(), "/");
        assert!(Rc::ptr_eq(
            &home_document,
            &browser
                .document()
                .expect("SPA document should remain loaded")
        ));
        assert_eq!(
            browser
                .document()
                .expect("SPA document should exist")
                .borrow()
                .element_text_content(status)
                .as_deref(),
            Some("/|none")
        );
        assert!(browser.forward().expect("history forward should run"));
        assert_eq!(browser.current_url().unwrap().path(), "/details");
        assert_eq!(
            browser
                .document()
                .expect("SPA document should exist")
                .borrow()
                .element_text_content(status)
                .as_deref(),
            Some("/details|details")
        );
    }

    #[test]
    fn history_replace_state_updates_the_current_entry() {
        let mut browser = Browser::new(bundle(
            r#"<button id="replace">replace</button><p id="status">home</p><script>
                const status = document.getElementById("status");
                document.getElementById("replace").addEventListener("click", () => {
                    history.replaceState({ view: "settings" }, "", "/settings");
                    status.textContent = window.history.state.view + "/" + window.history.length;
                });
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("home should load");
        assert!(
            browser
                .click_id("replace")
                .expect("replaceState should run")
        );
        assert_eq!(browser.current_url().unwrap().path(), "/settings");
        assert_eq!(browser.history().len(), 1);
        let document = browser.document().expect("SPA document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("settings/1")
        );
    }

    #[test]
    fn history_traversal_reloads_different_documents() {
        let mut resources =
            bundle(r#"<a id="about" href="/about.html">about</a><p id="status">home</p>"#);
        resources
            .insert(
                "/about.html",
                "text/html",
                br#"<p id="status">about</p>"#.to_vec(),
            )
            .expect("about page should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("home should load");
        assert!(
            browser
                .click_id("about")
                .expect("about link should activate")
        );
        assert_eq!(browser.current_url().unwrap().path(), "/about.html");
        assert!(browser.back().expect("history back should run"));
        assert_eq!(browser.current_url().unwrap().path(), "/");
        let document = browser.document().expect("home document should reload");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("home")
        );
        assert!(browser.forward().expect("history forward should run"));
        assert_eq!(browser.current_url().unwrap().path(), "/about.html");
        let document = browser.document().expect("about document should reload");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("about")
        );
    }

    #[test]
    fn storage_survives_full_document_navigation() {
        let mut resources = bundle(
            r#"<a id="next" href="/next.html">next</a><script>
                localStorage.setItem("token", "ready");
                sessionStorage.setItem("draft", "open");
            </script>"#,
        );
        resources
            .insert(
                "/next.html",
                "text/html",
                br#"<p id="status"></p><script>
                    const status = document.getElementById("status");
                    status.textContent = localStorage.getItem("token") + "|" + sessionStorage.getItem("draft");
                </script>"#
                    .to_vec(),
            )
            .expect("next page should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("home should load");
        assert!(browser.click_id("next").expect("next link should activate"));
        let document = browser.document().expect("next document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("ready|open")
        );
    }

    #[test]
    fn cache_storage_survives_full_document_navigation_without_a_loopback_server() {
        let mut resources = bundle(
            r#"<a id="next" href="/next.html">next</a><script>
                async function seed() {
                    const cache = await caches.open("shell");
                    await cache.put("/offline.txt", new Response("stored", { status: 203 }));
                }
                seed();
            </script>"#,
        );
        resources
            .insert(
                "/next.html",
                "text/html",
                br#"<p id="status"></p><script>
                    async function readCache() {
                        const response = await caches.match("/offline.txt");
                        document.getElementById("status").textContent = response.status + "|" + await response.text();
                    }
                    readCache();
                </script>"#
                    .to_vec(),
            )
            .expect("next page should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("home should load");
        assert!(browser.click_id("next").expect("next link should activate"));
        let document = browser.document().expect("next document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("203|stored")
        );
    }

    #[test]
    fn indexed_db_survives_full_document_navigation_without_a_loopback_server() {
        let mut resources = bundle(
            r#"<a id="next" href="/next.html">next</a><script>
                const open = indexedDB.open("notes", 1);
                open.onupgradeneeded = event => {
                    event.target.result.createObjectStore("notes", { keyPath: "id" });
                };
                open.onsuccess = event => {
                    event.target.result
                        .transaction("notes", "readwrite")
                        .objectStore("notes")
                        .put({ id: "persist", value: "yes" });
                };
            </script>"#,
        );
        resources
            .insert(
                "/next.html",
                "text/html",
                br#"<p id="status"></p><script>
                    const open = indexedDB.open("notes");
                    open.onsuccess = event => {
                        const request = event.target.result
                            .transaction("notes")
                            .objectStore("notes")
                            .get("persist");
                        request.onsuccess = event => {
                            document.getElementById("status").textContent = event.target.result.value;
                        };
                    };
                </script>"#
                    .to_vec(),
            )
            .expect("next page should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("home should load");
        assert!(browser.click_id("next").expect("next link should activate"));
        let document = browser.document().expect("next document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("yes")
        );
    }

    #[test]
    fn persistent_storage_survives_browser_restart_without_a_loopback_server() {
        let storage_root = std::env::temp_dir().join(format!(
            "ceri-engine-storage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after the epoch")
                .as_nanos()
        ));
        let seed_resources = bundle(
            r#"<script>
                localStorage.setItem("token", "disk");
                caches.open("shell").then(cache =>
                    cache.put("/offline.txt", new Response("cached", { status: 207 }))
                );
                const open = indexedDB.open("notes", 1);
                open.onupgradeneeded = event => {
                    event.target.result.createObjectStore("notes", { keyPath: "id" });
                };
                open.onsuccess = event => {
                    event.target.result
                        .transaction("notes", "readwrite")
                        .objectStore("notes")
                        .put({
                            id: "record",
                            value: "db",
                            bytes: new Uint8Array([1, 2]),
                            map: new Map([["kind", "structured"]]),
                            blob: new Blob(["blob"], { type: "text/plain" }),
                            file: new File(["file"], "report.txt", { type: "text/file" })
                        });
                };
            </script>"#,
        );
        let read_resources = bundle(
            r#"<p id="status"></p><script>
                const open = indexedDB.open("notes");
                open.onsuccess = event => {
                    const request = event.target.result
                        .transaction("notes")
                        .objectStore("notes")
                        .get("record");
                    request.onsuccess = event => {
                        caches.match("/offline.txt").then(response => {
                            const statusCode = response.status;
                            response.text().then(text => {
                                document.getElementById("status").textContent =
                                    localStorage.getItem("token") + "|" + event.target.result.value +
                                    "|" + event.target.result.bytes[1] + "|" +
                                    event.target.result.map.size + "|" + event.target.result.blob.type +
                                    "|" + event.target.result.file.name + "|" + statusCode + ":" + text;
                            });
                        });
                    };
                };
            </script>"#,
        );
        {
            let mut browser = Browser::new(seed_resources)
                .with_storage_directory(&storage_root)
                .expect("persistent storage should initialize");
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("seed page should load");
        }
        assert!(storage_root.join("storage.bin").is_file());
        {
            let mut browser = Browser::new(read_resources)
                .with_storage_directory(&storage_root)
                .expect("persistent storage should reopen");
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("read page should load");
            let document = browser.document().expect("read document should exist");
            let status = document
                .borrow()
                .find_element_by_id("status")
                .expect("status should exist");
            assert_eq!(
                document.borrow().element_text_content(status).as_deref(),
                Some("disk|db|2|1|text/plain|report.txt|207:cached")
            );
        }
        std::fs::remove_dir_all(&storage_root).expect("test storage should be cleaned up");
    }

    #[test]
    fn persistent_storage_rejects_corrupt_snapshots() {
        let storage_root = std::env::temp_dir().join(format!(
            "ceri-engine-storage-corrupt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after the epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&storage_root).expect("test storage root should be created");
        std::fs::write(storage_root.join("storage.bin"), b"corrupt")
            .expect("corrupt snapshot should be written");
        let result = Browser::new(bundle("<p>page</p>")).with_storage_directory(&storage_root);
        assert!(result.is_err());
        std::fs::remove_dir_all(&storage_root).expect("test storage should be cleaned up");
    }

    #[test]
    fn form_get_submission_navigates_packaged_origin_without_a_listener() {
        let mut resources = bundle(
            r#"<form id="search" action="/results.html" method="get"><input name="q" value="rust browser"><button id="submit" type="submit">search</button></form>"#,
        );
        resources
            .insert(
                "/results.html",
                "text/html",
                br#"<p id="status"></p><script>document.getElementById("status").textContent = location.search;</script>"#.to_vec(),
            )
            .expect("results page should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("search page should load");
        assert!(
            browser
                .click_id("submit")
                .expect("submit click should dispatch")
        );
        assert_eq!(browser.current_url().unwrap().path(), "/results.html");
        assert_eq!(browser.current_url().unwrap().query(), "q=rust+browser");
        let document = browser.document().expect("results document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("?q=rust+browser")
        );
    }

    #[test]
    fn form_post_submission_uses_provider_body_and_follows_a_same_origin_redirect() {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let provider = Rc::new(FormProvider {
            requests: requests.clone(),
            multipart: false,
        });
        let mut browser = Browser::with_resource_provider(ResourceBundle::default(), provider);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("form page should load");
        assert!(
            browser
                .click_id("submit")
                .expect("submit click should dispatch")
        );
        assert_eq!(browser.current_url().unwrap().path(), "/result.html");
        let document = browser.document().expect("result document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("posted")
        );

        let requests = requests.borrow();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].method(), "GET");
        assert_eq!(requests[1].method(), "POST");
        assert_eq!(
            requests[1].headers().get("content-type"),
            Some(&"application/x-www-form-urlencoded".to_string())
        );
        assert_eq!(requests[1].body(), b"q=rust+browser");
        assert_eq!(requests[2].method(), "GET");
        assert!(requests[2].body().is_empty());
    }

    #[test]
    fn multipart_form_submission_reuses_the_deterministic_form_data_encoder() {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let provider = Rc::new(FormProvider {
            requests: requests.clone(),
            multipart: true,
        });
        let mut browser = Browser::with_resource_provider(ResourceBundle::default(), provider);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("multipart form page should load");
        assert!(
            browser
                .click_id("submit")
                .expect("multipart submit should dispatch")
        );
        let document = browser.document().expect("multipart result should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("multipart status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("multipart posted")
        );

        let requests = requests.borrow();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].method(), "POST");
        let content_type = requests[1]
            .headers()
            .get("content-type")
            .expect("multipart content type should be present");
        assert_eq!(
            content_type,
            "multipart/form-data; boundary=----ceri-form-data-boundary"
        );
        let body = String::from_utf8(requests[1].body().to_vec())
            .expect("deterministic multipart body should be UTF-8 for text fields");
        assert!(body.contains("Content-Disposition: form-data; name=\"q\""));
        assert!(body.contains("\r\n\r\nrust browser\r\n"));
        assert!(body.ends_with("------ceri-form-data-boundary--\r\n"));
    }

    #[test]
    fn file_picker_selection_reaches_file_list_and_multipart_submission() {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let picker_requests = Rc::new(RefCell::new(Vec::new()));
        let selection =
            FileSelection::new("report.txt", "text/plain", b"file payload".to_vec(), 123)
                .expect("file selection should be valid");
        let picker = Rc::new(StaticFilePicker {
            requests: picker_requests.clone(),
            selection: Some(vec![selection]),
        });
        let provider = Rc::new(UploadProvider {
            requests: requests.clone(),
        });
        let mut browser = Browser::with_resource_provider(ResourceBundle::default(), provider)
            .with_file_picker(picker);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("upload page should load");
        assert!(
            browser
                .click_id("file")
                .expect("file picker should activate")
        );
        let document = browser.document().expect("upload document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("1|report.txt|text/plain|123")
        );
        let picker_requests = picker_requests.borrow();
        assert_eq!(picker_requests.len(), 1);
        assert_eq!(picker_requests[0].name(), "upload");
        assert!(!picker_requests[0].multiple());
        assert_eq!(picker_requests[0].accept(), Some("text/plain"));
        drop(picker_requests);

        assert!(browser.click_id("submit").expect("upload should submit"));
        let document = browser.document().expect("upload result should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("upload result status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("uploaded")
        );
        let requests = requests.borrow();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].method(), "POST");
        assert_eq!(
            requests[1].headers().get("content-type"),
            Some(&"multipart/form-data; boundary=----ceri-form-data-boundary".to_string())
        );
        let body = String::from_utf8(requests[1].body().to_vec())
            .expect("text upload body should be UTF-8");
        assert!(
            body.contains(
                "Content-Disposition: form-data; name=\"upload\"; filename=\"report.txt\""
            )
        );
        assert!(body.contains("Content-Type: text/plain\r\n\r\nfile payload\r\n"));
    }

    #[test]
    fn clipboard_provider_backing_navigator_clipboard_reads_and_writes_text() {
        let text = Rc::new(RefCell::new("initial".to_string()));
        let writes = Rc::new(RefCell::new(Vec::new()));
        let provider = Rc::new(MemoryClipboard {
            text: text.clone(),
            writes: writes.clone(),
        });
        let mut browser = Browser::new(bundle(
            r#"<p id="status">idle</p><script>
                const status = document.getElementById("status");
                const same = navigator.clipboard === window.navigator.clipboard;
                navigator.clipboard.writeText("copied").then(() => navigator.clipboard.readText()).then(value => {
                    status.textContent = typeof navigator + "|" + same + "|" + value;
                });
            </script>"#,
        ))
        .with_clipboard_provider(provider);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("clipboard page should load");
        let document = browser.document().expect("clipboard document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("clipboard status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("object|true|copied")
        );
        assert_eq!(writes.borrow().as_slice(), ["copied"]);
        assert_eq!(text.borrow().as_str(), "copied");
    }

    #[test]
    fn script_measures_layout_computed_style_and_media_queries() {
        let mut browser = Browser::new(bundle(
            r#"<style>body { margin: 0 } #box { position: relative; width: 100px; height: 40px; padding: 10px; border: 2px solid red; margin: 5px 0 0 8px; color: rgb(10, 20, 30) } #inner { height: 10px } @media (max-width: 500px) { #box { width: 50px } }</style><div id="box"><div id="inner"></div></div><p id="status">idle</p><script>
                const box = document.getElementById("box");
                const inner = document.getElementById("inner");
                const rect = box.getBoundingClientRect();
                const style = getComputedStyle(box);
                globalThis.wide = matchMedia("(min-width: 600px)");
                globalThis.changes = [];
                wide.addEventListener("change", (event) => changes.push(event.matches + ":" + event.media + ":" + wide.matches));
                wide.onchange = () => changes.push("handler");
                document.getElementById("status").textContent = [
                    rect.x, rect.y, rect.width, rect.height, box.offsetWidth, box.clientWidth,
                    box.clientTop, inner.offsetTop, inner.offsetParent === box, style.width,
                    style.paddingLeft, style.getPropertyValue("border-top-width"), style.color,
                    style.display, style.position, wide.matches, wide.media, wide instanceof EventTarget,
                ].join(",");
            </script>"#,
        ));
        browser.set_viewport(
            Size {
                width: 800.0,
                height: 600.0,
            },
            1.0,
        );
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("measuring page should load");
        let document = browser.document().expect("document should exist");
        let status = |document: &Rc<RefCell<Document>>| {
            let document = document.borrow();
            let node = document.find_element_by_id("status").expect("status");
            document.element_text_content(node)
        };
        assert_eq!(
            status(&document).as_deref(),
            Some(
                "8,5,124,64,124,120,2,10,true,100px,10px,2px,rgb(10, 20, 30),block,relative,true,(min-width: 600px),true"
            )
        );
        browser.set_viewport(
            Size {
                width: 400.0,
                height: 600.0,
            },
            1.0,
        );
        let page = browser.page.as_mut().expect("page should be loaded");
        page.runtime
            .execute(
                r#"document.getElementById("status").textContent = changes.join("|") + "," + document.getElementById("box").offsetWidth;"#,
            )
            .expect("measurement after resize should run");
        assert_eq!(
            status(&document).as_deref(),
            Some("handler|false:(min-width: 600px):false,74")
        );
    }

    #[test]
    fn scroll_sizes_count_end_padding_and_leave_out_borders() {
        let mut browser = Browser::new(bundle(
            r#"<style>body { margin: 0 } .scroller { overflow: auto; width: 100px; height: 100px; padding: 10px; border: 4px solid red } .block { width: 300px; height: 300px; margin: 0 7px 5px 0; flex: none } .line { display: inline-block; width: 300px; height: 20px } #plain { width: 50px; height: 20px; border: 3px solid red } #visible { position: relative; width: 50px; height: 20px; padding: 6px; border: 4px solid red } #visible > div { position: absolute; left: 0; top: 0; width: 80px; height: 30px; margin: 0 5px 5px 0 }</style><div class="scroller" id="block"><div class="block"></div></div><div class="scroller" id="flex" style="display: flex"><div class="block"></div></div><div class="scroller" id="line"><span class="line"></span></div><div id="plain"></div><div id="visible"><div></div></div><p id="status">idle</p><script>
                document.getElementById("status").textContent = ["block", "flex", "line", "plain", "visible"]
                    .map((id) => {
                        const box = document.getElementById(id);
                        return [box.scrollWidth, box.scrollHeight, box.clientWidth, box.clientHeight].join("x");
                    })
                    .join(",");
            </script>"#,
        ));
        browser.set_viewport(
            Size {
                width: 800.0,
                height: 600.0,
            },
            1.0,
        );
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("scroll size page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        // what chromium reports for the same markup
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some("327x325x105x105,327x325x105x105,320x105x120x105,50x20x50x20,80x32x62x32")
        );
    }

    #[test]
    fn performance_timeline_idle_callbacks_and_event_target_objects() {
        let mut browser = Browser::new(bundle(
            r#"<p id="status">idle</p><script>
                const seen = [];
                performance.mark("start");
                setTimeout(() => {
                    performance.mark("end");
                    const measure = performance.measure("span", "start", "end");
                    seen.push(performance.now(), measure.duration, performance.getEntriesByType("mark").length, typeof performance.timeOrigin);
                    requestIdleCallback((deadline) => {
                        seen.push("idle:" + deadline.timeRemaining() + ":" + deadline.didTimeout);
                        document.getElementById("status").textContent = seen.join(",");
                    });
                    cancelIdleCallback(requestIdleCallback(() => seen.push("cancelled")));
                }, 25);
                class Bus extends EventTarget {
                    ping() { return this.dispatchEvent(new CustomEvent("ping", { detail: 3, cancelable: true })); }
                }
                const bus = new Bus();
                bus.addEventListener("ping", (event) => { seen.push("ping:" + event.detail + ":" + (event.target === bus)); event.preventDefault(); });
                seen.push(bus instanceof EventTarget, bus.ping());
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("timing page should load");
        browser.advance_time(30).expect("timers should run");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some("ping:3:true,true,false,25,25,2,number,idle:50:false")
        );
    }

    #[test]
    fn resize_and_intersection_observers_report_at_the_rendering_step() {
        let mut browser = Browser::new(bundle(
            r#"<style>body { margin: 0 } #box { width: 100px; height: 50px; padding: 5px } #far { margin-top: 2000px; height: 10px }</style><div id="box"></div><div id="far"></div><p id="status">idle</p><script>
                globalThis.log = [];
                const resize = new ResizeObserver((entries) => entries.forEach((entry) =>
                    log.push("resize:" + entry.contentRect.width + "x" + entry.contentRect.height + ":" + entry.borderBoxSize[0].inlineSize)));
                resize.observe(document.getElementById("box"));
                const visible = new IntersectionObserver((entries) => entries.forEach((entry) =>
                    log.push("io:" + entry.target.id + ":" + entry.isIntersecting + ":" + entry.intersectionRatio)), { threshold: [0, 1] });
                visible.observe(document.getElementById("box"));
                visible.observe(document.getElementById("far"));
                globalThis.report = () => document.getElementById("status").textContent = log.join(",");
            </script>"#,
        ));
        browser.set_viewport(
            Size {
                width: 800.0,
                height: 600.0,
            },
            1.0,
        );
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("observer page should load");
        browser.advance_time(16).expect("first rendering step");
        let page = browser.page.as_mut().expect("page should be loaded");
        page.runtime
            .execute(r#"document.getElementById("box").style.width = "50px";"#)
            .expect("resize should apply");
        browser.advance_time(16).expect("second rendering step");
        browser.advance_time(16).expect("quiet rendering step");
        let page = browser.page.as_mut().expect("page should be loaded");
        page.runtime
            .execute("report();")
            .expect("report should run");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some("resize:100x50:110,io:box:true:1,io:far:false:0,resize:50x50:60")
        );
    }

    #[test]
    fn xml_http_request_loads_packaged_resources_from_a_task() {
        let mut resources = bundle(
            r#"<p id="status">idle</p><script>
                const log = [];
                const xhr = new XMLHttpRequest();
                xhr.onreadystatechange = () => log.push("state:" + xhr.readyState);
                xhr.addEventListener("load", (event) => log.push("load:" + xhr.status + ":" + xhr.response.answer + ":" + event.loaded + ":" + xhr.getResponseHeader("Content-Type")));
                xhr.onloadend = () => {
                    log.push("end:" + (xhr instanceof EventTarget) + ":" + XMLHttpRequest.DONE);
                    document.getElementById("status").textContent = log.join(",");
                };
                xhr.open("GET", "/data.json");
                xhr.responseType = "json";
                xhr.send();
                log.push("sent:" + xhr.readyState);
                const sync = new XMLHttpRequest();
                sync.open("GET", "/data.json", false);
                sync.send();
                log.push("sync:" + sync.status + ":" + sync.responseText.length);
                const missing = new XMLHttpRequest();
                missing.open("GET", "/missing.json", false);
                missing.send();
                log.push("missing:" + missing.status);
                try { missing.setRequestHeader("x", "y"); } catch (error) { log.push(error.name); }
            </script>"#,
        );
        resources
            .insert(
                "/data.json",
                "application/json",
                br#"{"answer":42}"#.to_vec(),
            )
            .expect("data should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("request page should load");
        browser
            .advance_time(0)
            .expect("the response task should run");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some(
                "state:1,sent:1,sync:200:13,missing:404,InvalidStateError,state:2,state:3,state:4,load:200:42:13:application/json,end:true:4"
            )
        );
    }

    #[test]
    fn shadow_roots_render_slots_scope_styles_and_retarget_events() {
        let mut browser = Browser::new(bundle(
            r#"<style>body { margin: 0 } .label { color: rgb(255, 0, 0) } x-card { display: block }</style><x-card id="card"><span class="label" id="title" slot="title">Title</span><span class="label" id="text">body text</span></x-card><p id="status">idle</p><script>
                const card = document.getElementById("card");
                const root = card.attachShadow({ mode: "open" });
                root.innerHTML = '<style>:host { padding: 4px } .label { color: rgb(0, 0, 255) } :host h2 { margin: 0; height: 20px }</style><h2 class="label"><slot name="title"></slot></h2><div id="inner"><slot></slot></div>';
                const heading = root.querySelector("h2");
                const inner = root.getElementById("inner");
                const log = [];
                card.addEventListener("click", (event) => log.push("host:" + event.target.id + ":" + event.composedPath().length));
                inner.addEventListener("click", (event) => log.push("inner:" + event.target.id));
                document.getElementById("text").click();
                heading.click();
                const style = (element) => getComputedStyle(element).color;
                document.getElementById("status").textContent = [
                    card.shadowRoot === root, root.host === card, root.mode, root instanceof ShadowRoot,
                    card.querySelector("h2"), card.children.length,
                    style(heading), style(document.getElementById("title")), getComputedStyle(card).paddingTop,
                    heading.offsetHeight, inner.offsetHeight > 0,
                    document.getElementById("title").assignedSlot === root.querySelector("slot[name=title]"),
                    root.querySelector("slot:not([name])").assignedNodes().length,
                    heading.getRootNode() === root, heading.getRootNode({ composed: true }) === document,
                    log.join("|"),
                ].join(",");
            </script>"#,
        ));
        browser.set_viewport(
            Size {
                width: 400.0,
                height: 300.0,
            },
            1.0,
        );
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("shadow page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some(
                "true,true,open,true,,2,rgb(0, 0, 255),rgb(255, 0, 0),4px,20,true,true,1,true,true,inner:text|host:text:9|host:card:7"
            )
        );
    }

    #[test]
    fn cascade_honours_importance_source_order_colors_and_level_four_selectors() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                body { margin: 0 }
                #a { color: rgb(10 20 30 / 50%); border: 2px solid currentColor; background-color: color-mix(in srgb, red, blue) }
                #a { margin-top: 3px; margin: 7px; }
                #a { margin-left: 9px }
                #a { padding-top: 1px !important } div#a { padding-top: 5px }
                .box:is(.x, .y) { width: 10px } .box:where(#a) { width: 99px }
                li:nth-child(2n of .odd) { color: hsl(120 100% 25%) }
                ul:has(> li.odd) { color: rebeccapurple }
            </style>
            <div id="a" class="box x" data-x="one two" lang="en-GB" data-y="abc">box</div>
            <ul id="list"><li class="odd">1</li><li>2</li><li class="odd" id="third">3</li><li></li></ul>
            <input id="field" placeholder="type"><a id="link" href="https://ceri.example/doc.pdf">doc</a>
            <p id="status">idle</p>
            <script>
                const a = document.getElementById("a");
                const style = getComputedStyle(a);
                const checks = [
                    style.color, style.borderTopColor, style.backgroundColor, style.marginTop, style.marginLeft,
                    style.paddingTop, style.width,
                    getComputedStyle(document.getElementById("third")).color,
                    getComputedStyle(document.getElementById("list")).color,
                ];
                const selectors = [
                    ["#a", "[data-x~=two]"], ["#a", "[lang|=en]"], ["#a", "[data-y=ABC i]"], ["#a", "[data-y=ABC]"],
                    ["#link", "[href^=https][href$='.pdf'][href*=ceri]"], ["#link", "a:any-link"], ["#link", "a:visited"],
                    ["#field", "input:placeholder-shown:read-write:optional"], ["#list", "ul:has(> li:empty)"],
                    ["#third", "li:last-of-type"], ["#third", "li:nth-last-child(2)"], ["#third", "li:nth-of-type(3)"],
                    ["#list", ":not(:has(+ p))"], ["#a", "div:is(.nope, .x):not(.y)"],
                ];
                for (const [id, selector] of selectors) checks.push(document.querySelector(id).matches(selector));
                document.getElementById("status").textContent = checks.join("|");
            </script>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("cascade page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document
            .find_element_by_id("status")
            .unwrap_or_else(|| panic!("{:?}", browser.last_navigation_failure()));
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some(
                "rgba(10, 20, 30, 0.502)|rgba(10, 20, 30, 0.502)|rgb(128, 0, 128)|7px|9px|1px|10px|rgb(0, 128, 0)|rgb(102, 51, 153)|true|true|true|false|true|true|false|true|true|false|true|true|true|true"
            )
        );
    }

    #[test]
    fn block_layout_sizes_boxes_centers_auto_margins_and_collapses_margins() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                body { margin: 0 }
                #c { width: 300px }
                .bb { box-sizing: border-box; width: 100px; padding: 10px; border: 5px solid black }
                .center { width: 100px; margin: 0 auto }
                .max { max-width: 120px; margin-left: auto; margin-right: auto }
                .min { width: 10px; min-width: 50px }
                .a { margin-bottom: 20px; height: 10px }
                .b { margin-top: 30px; height: 10px }
                .neg { margin-top: -5px; height: 10px }
                #fixed { height: 200px }
                .pct { height: 50% }
            </style>
            <div id="c"><div id="bb" class="bb">x</div><div id="center" class="center"></div><div id="max" class="max"></div><div id="min" class="min"></div><div id="a" class="a"></div><div id="b" class="b"></div><div id="neg" class="neg"></div></div>
            <div id="fixed"><div id="pct" class="pct"></div></div><div id="loose"><div id="pct2" class="pct"></div></div>
            <p id="status">idle</p>
            <script>
                const box = (id) => document.getElementById(id).getBoundingClientRect();
                document.getElementById("status").textContent = [
                    box("bb").width, box("bb").height, box("center").x, box("max").width, box("max").x,
                    box("min").width, box("a").y, box("b").y, box("neg").y, box("pct").height, box("pct2").height,
                ].join(",");
            </script>"##,
        ));
        browser.set_viewport(
            Size {
                width: 400.0,
                height: 600.0,
            },
            1.0,
        );
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("box model page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some("100,50,100,120,90,50,50,90,95,100,0")
        );
    }

    #[test]
    fn flex_layout_grows_shrinks_wraps_aligns_and_wraps_text_in_anonymous_items() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                body { margin: 0 }
                .row { display: flex; width: 300px }
                .row > div { height: 10px }
                #grow > .a { flex: 1 } #grow > .b { flex: 2 }
                #shrink > div { width: 200px } #shrink > .c { flex-shrink: 0 }
                #wrap { flex-wrap: wrap; width: 100px; gap: 5px 10px } #wrap > div { width: 40px }
                #just { justify-content: space-between } #just > div { width: 50px }
                #evenly { justify-content: space-evenly } #evenly > div { width: 60px }
                #auto > .push { margin-left: auto; width: 50px }
                #col { display: flex; flex-direction: column; height: 100px; width: 100px } #col > .g { flex: 1 }
                #rev { flex-direction: row-reverse } #rev > div { width: 50px }
                #order > div { width: 30px } #order > .first { order: -1 }
                #center { align-items: center; height: 50px } #center > div { height: 20px; width: 20px }
                #text { display: flex; width: 300px; gap: 10px }
            </style>
            <div class="row" id="grow"><div class="a" id="g1"></div><div class="b" id="g2"></div></div>
            <div class="row" id="shrink"><div id="s1"></div><div class="c" id="s2"></div></div>
            <div class="row" id="wrap"><div id="w1"></div><div id="w2"></div><div id="w3"></div></div>
            <div class="row" id="just"><div id="j1"></div><div id="j2"></div><div id="j3"></div></div>
            <div class="row" id="evenly"><div id="e1"></div><div id="e2"></div></div>
            <div class="row" id="auto"><div id="a1" class="push"></div></div>
            <div id="col"><div id="c1" style="height: 20px"></div><div class="g" id="c2"></div></div>
            <div class="row" id="rev"><div id="r1"></div><div id="r2"></div></div>
            <div class="row" id="order"><div id="o1"></div><div class="first" id="o2"></div></div>
            <div class="row" id="center"><div id="ce"></div></div>
            <div id="text">hello<span id="after">x</span></div>
            <p id="status">idle</p>
            <script>
                const box = (id) => document.getElementById(id).getBoundingClientRect();
                const x = (id, parent) => box(id).x - box(parent).x;
                const y = (id, parent) => box(id).y - box(parent).y;
                document.getElementById("status").textContent = [
                    box("g1").width, box("g2").width, x("g2", "grow"),
                    box("s1").width, x("s2", "shrink"),
                    x("w3", "wrap"), y("w3", "wrap"), box("wrap").height,
                    x("j2", "just"), x("j3", "just"),
                    x("e1", "evenly"), x("e2", "evenly"),
                    x("a1", "auto"),
                    y("c2", "col"), box("c2").height, box("c1").width,
                    x("r1", "rev"), x("r2", "rev"),
                    x("o2", "order"), x("o1", "order"),
                    y("ce", "center"),
                    x("after", "text") > 10, box("text").height,
                ].join(",");
            </script>"##,
        ));
        browser.set_viewport(
            Size {
                width: 400.0,
                height: 600.0,
            },
            1.0,
        );
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("flex page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some(
                "100,200,100,100,100,0,15,25,125,250,60,180,250,20,80,100,250,200,0,30,15,true,20"
            )
        );
    }

    #[test]
    fn grid_layout_places_items_by_line_span_and_area_and_sizes_tracks() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                body { margin: 0 }
                .g { display: grid }
                .g > div { height: 10px }
                #fr { grid-template-columns: 100px 1fr 2fr; width: 420px; column-gap: 10px }
                #place { grid-template-columns: repeat(3, 50px); grid-auto-rows: 20px; width: 150px }
                #p1 { grid-column: 2 / span 2; grid-row: 2 } #p3 { grid-column: -2 }
                #areas { grid-template-areas: "head head" "side main"; grid-template-columns: 80px 1fr; width: 200px }
                #areas > #head { grid-area: head; height: 30px } #areas > #side { grid-area: side; height: 40px }
                #areas > #main { grid-area: main; height: auto }
                #fill { grid-template-columns: repeat(auto-fill, minmax(60px, 1fr)); width: 215px; gap: 10px }
                #fit { grid-template-columns: repeat(auto-fit, 50px); width: 300px; justify-content: center }
                #flow { grid-template-columns: 50px; grid-template-rows: 10px 10px; grid-auto-flow: column; grid-auto-columns: 30px; width: 200px }
                #text { grid-template-columns: 100px 100px; width: 200px }
                #align { grid-template-columns: 100px; grid-template-rows: 50px; justify-items: center; align-items: end }
                #align > div { width: 20px }
                #content { grid-template-columns: auto 1fr; width: 300px }
            </style>
            <div class="g" id="fr"><div id="f1"></div><div id="f2"></div><div id="f3"></div></div>
            <div class="g" id="place"><div id="p1"></div><div id="p2"></div><div id="p3"></div></div>
            <div class="g" id="areas"><div id="main"></div><div id="side"></div><div id="head"></div></div>
            <div class="g" id="fill"><div id="l1"></div><div id="l2"></div><div id="l3"></div><div id="l4"></div></div>
            <div class="g" id="fit"><div id="t1"></div><div id="t2"></div></div>
            <div class="g" id="flow"><div id="i1"></div><div id="i2"></div><div id="i3"></div></div>
            <div class="g" id="text">hello<span id="tg">x</span></div>
            <div class="g" id="align"><div id="al"></div></div>
            <div class="g" id="content"><div><div style="width: 50px"></div></div><div id="c2"></div></div>
            <p id="status">idle</p>
            <script>
                const box = (id) => document.getElementById(id).getBoundingClientRect();
                const x = (id, parent) => box(id).x - box(parent).x;
                const y = (id, parent) => box(id).y - box(parent).y;
                document.getElementById("status").textContent = [
                    x("f3", "fr"), box("f2").width,
                    x("p1", "place"), y("p1", "place"), box("p1").width,
                    x("p2", "place"), y("p2", "place"), x("p3", "place"),
                    box("head").width, y("main", "areas"), x("main", "areas"),
                    box("main").height, box("main").width,
                    box("l1").width, x("l3", "fill"), x("l4", "fill"), y("l4", "fill"),
                    x("t1", "fit"),
                    x("i3", "flow"), box("i3").width,
                    x("tg", "text"),
                    x("al", "align"), y("al", "align"),
                    x("c2", "content"),
                ].join(",");
            </script>"##,
        ));
        browser.set_viewport(
            Size {
                width: 400.0,
                height: 600.0,
            },
            1.0,
        );
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("grid page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some("220,100,50,20,100,0,0,100,200,30,80,40,120,65,150,0,20,100,50,30,100,40,40,50")
        );
    }

    #[test]
    fn selection_keeps_its_anchor_through_set_base_and_extent_and_extend() {
        let mut browser = Browser::new(bundle(
            r#"<p id="a">hello</p><p id="b">world</p><p id="status">idle</p><script>
                const selection = window.getSelection();
                const a = document.getElementById("a").firstChild;
                const b = document.getElementById("b").firstChild;
                selection.setBaseAndExtent(b, 3, a, 1);
                let report = selection.direction + "|" + (selection.anchorNode === b) + "|" + selection.anchorOffset + "|" + (selection.focusNode === a) + "|" + selection.focusOffset + "|" + selection.toString();
                selection.extend(b, 5);
                report = report + "|" + selection.direction + "|" + selection.anchorOffset + "|" + selection.toString();
                selection.collapse(a, 2);
                report = report + "|" + selection.direction;
                document.getElementById("status").textContent = report;
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("selection page should load");
        let document = browser.document().expect("selection document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .unwrap_or_else(|| panic!("{:?}", browser.last_navigation_failure()));
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("backward|true|3|true|1|ellowor|forward|3|ld|none")
        );
    }

    #[test]
    fn clipboard_without_a_host_provider_rejects_without_a_listener() {
        let mut browser = Browser::new(bundle(
            r#"<p id="status">idle</p><script>
                navigator.clipboard.readText().catch(error => {
                    document.getElementById("status").textContent = error;
                });
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("clipboard page should load");
        let document = browser.document().expect("clipboard document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("clipboard status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("NotAllowedError: clipboard access is unavailable")
        );
    }

    #[test]
    fn clipboard_provider_permission_errors_reach_the_script_promise() {
        let mut browser = Browser::new(bundle(
            r#"<p id="status">idle</p><script>
                navigator.clipboard.readText().catch(error => {
                    document.getElementById("status").textContent = error;
                });
            </script>"#,
        ))
        .with_clipboard_provider(Rc::new(DeniedClipboard));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("clipboard page should load");
        let document = browser.document().expect("clipboard document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("clipboard status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("NotAllowedError: clipboard failed: permission denied")
        );
    }

    #[test]
    fn packaged_post_submission_uses_an_explicit_recovery_page_without_a_provider() {
        let mut browser = Browser::new(bundle(
            r#"<form action="/submit" method="post"><button id="submit" type="submit">send</button></form>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("form page should load");
        assert!(
            browser
                .click_id("submit")
                .expect("submit click should dispatch")
        );
        assert_eq!(
            browser
                .last_navigation_failure()
                .expect("provider boundary failure should be recorded")
                .status,
            503
        );
        let document = browser.document().expect("recovery document should exist");
        let recovery = document
            .borrow()
            .find_element_by_id("recovery")
            .expect("recovery element should exist");
        assert_eq!(
            document
                .borrow()
                .attribute(recovery, "data-status")
                .map(str::to_string)
                .as_deref(),
            Some("503")
        );
    }

    #[test]
    fn location_redirects_are_bounded() {
        let mut browser = Browser::new(bundle(r#"<script>location.href = "/";</script>"#));
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect_err("redirect loop should be rejected"),
            NavigationError::NavigationLoop
        );
    }

    #[test]
    fn extensionless_navigation_uses_the_packaged_app_shell() {
        let mut browser = Browser::new(bundle("<p id='status'>shell</p>"));
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/projects/ceri")
                .expect("SPA route should load"),
            NavigationStatus::Loaded
        );
        assert_eq!(browser.current_url().unwrap().path(), "/projects/ceri");
        let document = browser.document().unwrap();
        assert_eq!(
            document
                .borrow()
                .element_text_content(document.borrow().node_at(&[0, 1]))
                .as_deref(),
            Some("shell")
        );
    }

    #[test]
    fn missing_resources_install_an_app_owned_recovery_document() {
        let mut browser = Browser::new(bundle("<p>home</p>"));
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/missing.js")
                .expect("missing resource should become recovery page"),
            NavigationStatus::ErrorPage { status: 404 }
        );
        let document = browser.document().unwrap();
        let recovery = document
            .borrow()
            .find_element_by_id("recovery")
            .expect("recovery element should exist");
        assert_eq!(
            document
                .borrow()
                .attribute(recovery, "data-status")
                .map(str::to_string),
            Some("404".to_string())
        );
        let failure = browser
            .last_navigation_failure()
            .expect("missing resource should expose a diagnostic");
        assert_eq!(failure.url.path(), "/missing.js");
        assert_eq!(failure.status, 404);
        assert_eq!(failure.detail, "resource not found: /missing.js");
    }

    #[test]
    fn provider_failures_install_recovery_and_expose_a_typed_diagnostic() {
        let provider = Rc::new(FailingProvider);
        let mut browser = Browser::with_resource_provider(ResourceBundle::default(), provider);
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("provider failure should become recovery"),
            NavigationStatus::ErrorPage { status: 503 }
        );
        let failure = browser
            .last_navigation_failure()
            .expect("provider failure should expose a diagnostic");
        assert_eq!(failure.url.path(), "/");
        assert_eq!(failure.status, 503);
        assert_eq!(
            failure.detail,
            "resource provider failed: transport unavailable"
        );
    }

    #[test]
    fn provider_http_status_reaches_recovery_without_shell_fallback() {
        let paths = Rc::new(RefCell::new(Vec::new()));
        let provider = Rc::new(StatusProvider {
            status: 404,
            paths: paths.clone(),
        });
        let mut browser = Browser::with_resource_provider(ResourceBundle::default(), provider);
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/dashboard")
                .expect("provider status should become recovery"),
            NavigationStatus::ErrorPage { status: 404 }
        );
        assert_eq!(&*paths.borrow(), &["/dashboard".to_string()]);
        let failure = browser
            .last_navigation_failure()
            .expect("provider status should expose a diagnostic");
        assert_eq!(failure.status, 404);
        assert_eq!(
            failure.detail,
            "resource request returned HTTP 404: /dashboard"
        );
    }

    #[test]
    fn provider_relative_redirects_follow_with_one_history_entry() {
        let paths = Rc::new(RefCell::new(Vec::new()));
        let provider = Rc::new(RedirectProvider {
            location: "/login".to_string(),
            paths: paths.clone(),
        });
        let mut browser = Browser::with_resource_provider(ResourceBundle::default(), provider);
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("provider redirect should resolve"),
            NavigationStatus::Loaded
        );
        assert_eq!(browser.current_url().unwrap().path(), "/login");
        assert_eq!(browser.history().len(), 1);
        assert_eq!(&*paths.borrow(), &["/".to_string(), "/login".to_string()]);
        let document = browser
            .document()
            .expect("redirected document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status element should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("login")
        );
    }

    #[test]
    fn provider_cross_origin_redirects_are_rejected() {
        let provider = Rc::new(RedirectProvider {
            location: "https://other.invalid/login".to_string(),
            paths: Rc::new(RefCell::new(Vec::new())),
        });
        let mut browser = Browser::with_resource_provider(ResourceBundle::default(), provider);
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect_err("cross-origin provider redirect should be rejected"),
            NavigationError::WrongOrigin
        );
    }

    #[test]
    fn provider_redirect_loops_stop_at_the_navigation_limit() {
        let paths = Rc::new(RefCell::new(Vec::new()));
        let provider = Rc::new(RedirectProvider {
            location: "/".to_string(),
            paths: paths.clone(),
        });
        let mut browser = Browser::with_resource_provider(ResourceBundle::default(), provider);
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect_err("provider redirect loop should be bounded"),
            NavigationError::NavigationLoop
        );
        assert_eq!(paths.borrow().len(), MAX_NAVIGATION_REDIRECTS + 1);
    }

    #[test]
    fn provider_module_redirect_updates_relative_import_base() {
        let mut browser = Browser::with_resource_provider(
            ResourceBundle::default(),
            Rc::new(ModuleRedirectProvider),
        );
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("redirected module should load"),
            NavigationStatus::Loaded
        );
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status element should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("redirected")
        );
    }

    #[test]
    fn provider_shell_fallback_preserves_the_requested_app_url() {
        let mut browser =
            Browser::with_resource_provider(ResourceBundle::default(), Rc::new(ShellProvider));
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/dashboard")
                .expect("provider shell should load"),
            NavigationStatus::Loaded
        );
        assert_eq!(browser.current_url().unwrap().path(), "/dashboard");
    }

    #[test]
    fn script_failures_are_contained_by_a_recovery_document() {
        let mut browser = Browser::new(bundle("<script>missing?</script>"));
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("script failure should become recovery page"),
            NavigationStatus::ErrorPage { status: 500 }
        );
        let document = browser.document().unwrap();
        let recovery = document
            .borrow()
            .find_element_by_id("recovery")
            .expect("recovery element should exist");
        assert_eq!(
            document
                .borrow()
                .attribute(recovery, "data-status")
                .map(str::to_string),
            Some("500".to_string())
        );
    }

    #[test]
    fn browser_dispatches_events_after_navigation() {
        let mut browser = Browser::new(bundle(
            r##"<button id="go">run</button><p id="status">idle</p><script>
                const button = document.getElementById("go");
                const status = document.getElementById("status");
                button.addEventListener("click", () => {
                    status.textContent = "clicked";
                    status.style.color = "#42d3b4";
                });
            </script>"##,
        ));
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("page should load"),
            NavigationStatus::Loaded
        );
        assert!(browser.click_id("go").expect("click should dispatch"));
        let document = browser.document().unwrap();
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status element should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("clicked")
        );
        assert_eq!(
            document.borrow().style_property(status, "color").as_deref(),
            Some("#42d3b4")
        );
    }

    #[test]
    fn browser_dispatches_coordinate_clicks_through_layout_paths() {
        let mut browser = Browser::new(bundle(
            r##"<main><button id="go" style="display: block; width: 120px; height: 40px">run</button><p id="status">idle</p></main><script>
                const button = document.getElementById("go");
                const status = document.getElementById("status");
                button.addEventListener("click", () => { status.textContent = "clicked"; });
            </script>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        assert!(
            browser
                .click_at(
                    Size {
                        width: 320.0,
                        height: 180.0,
                    },
                    10.0,
                    10.0,
                )
                .expect("coordinate click should dispatch")
        );
        let document = browser.document().unwrap();
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status element should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("clicked")
        );
    }

    #[test]
    fn layouts_are_reused_until_the_document_or_its_state_changes() {
        let mut browser = Browser::new(bundle(
            r##"<style>#box { width: 50px; height: 20px } #box:hover { width: 90px }</style>
            <div id="box"></div><p id="text">one</p>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let viewport = Size {
            width: 320.0,
            height: 180.0,
        };
        fn width_of(layout: &crate::layout::LayoutBox, node: NodeId) -> Option<f32> {
            if layout.node == node {
                return Some(layout.rect.width);
            }
            layout
                .children
                .iter()
                .find_map(|child| width_of(child, node))
        }
        let target = browser
            .document()
            .unwrap()
            .borrow()
            .find_element_by_id("box")
            .unwrap();
        let width = |layout: &LayoutDocument| width_of(&layout.root, target).unwrap();
        let first = browser.layout(viewport).expect("layout should build");
        assert!(Rc::ptr_eq(&first, &browser.layout(viewport).unwrap()));
        assert_eq!(width(&first), 50.0);
        browser
            .dispatch_at(viewport, 10.0, 10.0, "pointermove")
            .expect("pointer move should dispatch");
        let hovered = browser.layout(viewport).unwrap();
        assert_eq!(width(&hovered), 90.0);
        let document = browser.document().unwrap();
        let text = document.borrow().find_element_by_id("text").unwrap();
        document
            .borrow_mut()
            .set_attribute(text, "hidden", String::new());
        assert!(!Rc::ptr_eq(&hovered, &browser.layout(viewport).unwrap()));
    }

    #[test]
    fn persistent_storage_rewrites_its_file_only_when_the_store_changed() {
        let storage_root = std::env::temp_dir().join(format!(
            "ceri-engine-storage-unchanged-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after the epoch")
                .as_nanos()
        ));
        let file = storage_root.join("storage.bin");
        let mut browser = Browser::new(bundle(
            r##"<button id="go" style="display: block; width: 120px; height: 40px">save</button><script>
                document.getElementById("go").addEventListener("click", () => {
                    localStorage.setItem("token", "saved");
                });
            </script>"##,
        ))
        .with_storage_directory(&storage_root)
        .expect("persistent storage should initialize");
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        assert!(file.is_file());
        std::fs::remove_file(&file).expect("snapshot should be removable");
        browser.pump().expect("idle pump should succeed");
        assert!(!file.exists(), "an unchanged store should not be rewritten");
        let viewport = Size {
            width: 320.0,
            height: 180.0,
        };
        assert!(
            browser
                .click_at(viewport, 10.0, 10.0)
                .expect("click should dispatch")
        );
        assert!(file.is_file(), "a changed store should be written");
        let saved = std::fs::read(&file).expect("snapshot should be readable");
        assert!(saved.windows(5).any(|bytes| bytes == b"saved"));
        drop(browser);
        std::fs::remove_dir_all(&storage_root).expect("test storage should be cleaned up");
    }

    #[test]
    fn browser_dispatches_coordinate_clicks_to_positioned_overlays() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                #stage { position: relative; width: 200px; height: 100px; margin: 0; }
                #overlay { position: absolute; left: 30px; top: 20px; width: 40px; height: 20px; margin: 0; }
            </style>
            <div id="stage"><button id="overlay">run</button><p id="status">idle</p></div>
            <script>
                document.getElementById("overlay").addEventListener("click", () => {
                    document.getElementById("status").textContent = "overlay clicked";
                });
            </script>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        assert!(
            browser
                .click_at(
                    Size {
                        width: 320.0,
                        height: 180.0,
                    },
                    35.0,
                    25.0,
                )
                .expect("overlay click should dispatch")
        );
        let document = browser.document().unwrap();
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status element should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("overlay clicked")
        );
    }

    #[test]
    fn browser_hits_relative_offsets_and_z_order_at_visual_coordinates() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                #relative { position: relative; left: 30px; top: 10px; display: block; width: 40px; height: 20px; margin: 0; z-index: 1; }
                #low { position: absolute; left: 100px; top: 20px; width: 40px; height: 20px; margin: 0; z-index: 1; background: #ff0000; }
                #high { position: absolute; left: 100px; top: 20px; width: 40px; height: 20px; margin: 0; z-index: 2; background: #0000ff; }
            </style>
            <button id="relative">relative</button>
            <div id="low"></div>
            <button id="high">high</button>
            <p id="status">idle</p>
            <script>
                document.getElementById("relative").addEventListener("click", () => {
                    document.getElementById("status").textContent = "relative";
                });
                document.getElementById("low").addEventListener("click", () => {
                    document.getElementById("status").textContent = "low";
                });
                document.getElementById("high").addEventListener("click", () => {
                    document.getElementById("status").textContent = "high";
                });
            </script>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        assert!(
            browser
                .click_at(
                    Size {
                        width: 240.0,
                        height: 120.0,
                    },
                    35.0,
                    15.0,
                )
                .expect("relative visual click should dispatch")
        );
        let document = browser.document().unwrap();
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("relative")
        );
        assert!(
            browser
                .click_at(
                    Size {
                        width: 240.0,
                        height: 120.0,
                    },
                    110.0,
                    25.0,
                )
                .expect("top z-index click should dispatch")
        );
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("high")
        );
    }

    #[test]
    fn browser_keeps_nested_stacking_contexts_atomic_for_paint_and_input() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                #low { position: relative; z-index: 1; width: 30px; height: 30px; margin: 0; background: #ff0000; }
                #low-child { position: absolute; left: 0; top: 0; width: 30px; height: 30px; margin: 0; z-index: 100; background: #0000ff; }
                #high { position: relative; z-index: 2; top: -30px; width: 30px; height: 30px; margin: 0; background: #00ff00; }
            </style>
            <div id="low"><div id="low-child"></div></div>
            <button id="high">high</button>
            <p id="status">idle</p>
            <script>
                document.getElementById("low-child").addEventListener("click", () => {
                    document.getElementById("status").textContent = "low";
                });
                document.getElementById("high").addEventListener("click", () => {
                    document.getElementById("status").textContent = "high";
                });
            </script>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("stacking page should load");
        let viewport = Size {
            width: 80.0,
            height: 80.0,
        };
        let canvas = browser.paint(viewport).expect("stacking page should paint");
        // below the button's label, where only its background shows
        let pixel = &canvas.rgba_pixels()[(27 * canvas.width() as usize + 4) * 4..];
        assert_eq!(&pixel[..4], &[0, 255, 0, 255]);
        assert!(
            browser
                .click_at(viewport, 10.0, 10.0)
                .expect("stacking click should dispatch")
        );
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("high")
        );
    }

    #[test]
    fn font_face_rules_load_woff2_faces_relative_to_their_stylesheet() {
        let mut resources = bundle(
            r##"<link rel="stylesheet" href="/styles/fonts.css">
            <style>body { margin: 0 } .test { font-family: "Ceri Web"; font-size: 20px; display: inline-block }</style>
            <div><span class="test" id="web">A A</span><span class="test" id="missing" style="font-family: Nowhere">A A</span></div>
            <p id="status">idle</p>
            <script>
                document.getElementById("status").textContent = [
                    document.getElementById("web").getBoundingClientRect().width,
                    document.getElementById("missing").getBoundingClientRect().width,
                ].join(",");
            </script>"##,
        );
        resources
            .insert(
                "/styles/fonts.css",
                "text/css",
                b"@font-face { font-family: 'Ceri Web'; src: url(missing.woff2) format('woff2'), url(../fonts/ceri-test.woff2) format('woff2'); }"
                    .to_vec(),
            )
            .expect("stylesheet should be accepted");
        let font = std::fs::read(format!(
            "{}/tests/fixtures/fonts/ceri-test.woff2",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("fixture font should exist");
        resources
            .insert("/fonts/ceri-test.woff2", "font/woff2", font)
            .expect("font should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        // two 600 unit letters and a 300 unit space at 20px from the web font; a family with
        // no face borrows the web font's letters and keeps the builtin space
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some("30,36")
        );
    }

    #[test]
    fn text_gate_page_paints_web_fonts_shaping_colour_decoration_and_wrapping() {
        let mut resources = bundle(
            r##"<html><head><style>
                @font-face { font-family: "Ceri Web"; src: url(/fonts/ceri-test.woff2) format("woff2") }
                @font-face { font-family: "Ceri Color"; src: url(/fonts/ceri-color.ttf) }
                body { margin: 0; background: #ffffff; color: #000000; font: 20px/30px "Ceri Web" }
                .row { position: absolute; left: 0; height: 30px; white-space: nowrap }
                .box { display: inline-block }
            </style></head><body>
            <div class="row" style="top: 0"><span class="box" id="liga">fi</span></div>
            <div class="row" style="top: 30px"><span class="box" id="spaced" style="letter-spacing: 0.01px">fi</span></div>
            <div class="row" style="top: 60px"><span class="box" id="kern">AB</span></div>
            <div class="row" style="top: 90px; font-family: 'Ceri Color'; color: #0000ff">&#x263A;&#xFE0F;</div>
            <div class="row" style="top: 120px"><span style="text-decoration: underline #ff0000; text-shadow: 3px 3px 0 #00ff00">AB</span></div>
            <div class="row" id="cut" style="top: 160px; width: 60px; overflow: hidden; text-overflow: ellipsis">ABABABABAB</div>
            <div style="position: absolute; top: 200px; left: 0; width: 300px">
                <span class="box" id="collapsed">A   A</span>
                <span class="box" id="pre" style="white-space: pre">A   A</span>
                <div id="prewrap" style="white-space: pre-wrap; width: 40px">AA AA</div>
                <div id="preline" style="white-space: pre-line">A
A</div>
                <div id="url" style="width: 50px; overflow-wrap: anywhere">AAAAAAAAAAAAAAAA</div>
                <div id="nowrapurl" style="width: 50px">AAAAAAAAAAAAAAAA</div>
            </div>
            <p id="status" style="position: absolute; top: 600px">idle</p>
            <script>
                const rect = (id) => document.getElementById(id).getBoundingClientRect();
                document.getElementById("status").textContent = [
                    rect("liga").width, rect("spaced").width, rect("kern").width,
                    rect("collapsed").width, rect("pre").width,
                    rect("prewrap").height, rect("preline").height,
                    rect("url").height, rect("nowrapurl").height,
                ].map((value) => Math.round(value)).join(",");
            </script></body></html>"##,
        );
        for name in ["ceri-test.woff2", "ceri-color.ttf"] {
            let font = std::fs::read(format!(
                "{}/tests/fixtures/fonts/{name}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .expect("fixture font should exist");
            resources
                .insert(&format!("/fonts/{name}"), "font/ttf", font)
                .expect("font should be accepted");
        }
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        {
            let document = browser.document().expect("document should exist");
            let document = document.borrow();
            let status = document.find_element_by_id("status").expect("status");
            // the fi ligature is 700 units against 600 + 600 spaced apart, AB kerns by 100, pre
            // keeps the three spaces that collapse elsewhere, pre-wrap and pre-line and
            // overflow-wrap give two, two and four lines, and an unbreakable word stays on one
            assert_eq!(
                document.element_text_content(status).as_deref(),
                Some("14,24,22,30,42,60,60,120,30")
            );
        }
        let canvas = browser
            .paint(Size {
                width: 320.0,
                height: 640.0,
            })
            .expect("page should paint");
        let width = canvas.width() as usize;
        let pixel = |x: usize, y: usize| {
            let index = (y * width + x) * 4;
            let rgba = &canvas.rgba_pixels()[index..index + 3];
            [rgba[0], rgba[1], rgba[2]]
        };
        let dark_columns = |top: usize| {
            (0..40)
                .filter(|&x| (top..top + 30).any(|y| pixel(x, y) == [0, 0, 0]))
                .collect::<Vec<_>>()
        };
        // the ligature's bar runs unbroken over both letters; spaced apart the i stands alone
        let ligature = dark_columns(0);
        let spaced = dark_columns(30);
        assert_ne!(ligature, spaced, "ligatures change the painted glyphs");
        assert!(ligature.iter().all(|&x| x < 14) && spaced.iter().any(|&x| x >= 14));
        assert!(
            dark_columns(60).iter().all(|&x| x < 22),
            "kerned glyphs paint inside their advance"
        );
        let has = |top: usize, bottom: usize, color: [u8; 3]| {
            (top..bottom).any(|y| (0..60).any(|x| pixel(x, y) == color))
        };
        assert!(
            has(90, 120, [255, 0, 0]),
            "the colour glyph paints its palette red"
        );
        assert!(
            has(90, 120, [0, 0, 255]),
            "and its foreground layer in the text colour"
        );
        assert!(has(120, 160, [255, 0, 0]), "the underline paints");
        assert!(has(120, 160, [0, 255, 0]), "the shadow paints");
        let layout = browser
            .layout(Size {
                width: 320.0,
                height: 640.0,
            })
            .expect("page should lay out");
        let mut stack = vec![&layout.root];
        let mut ellipsis = false;
        while let Some(layout_box) = stack.pop() {
            ellipsis |= layout_box
                .text
                .iter()
                .any(|run| run.text.ends_with('\u{2026}'));
            stack.extend(layout_box.children.iter());
        }
        assert!(ellipsis, "the clipped nowrap row ends in an ellipsis");
        assert!(
            (160..190).all(|y| (60..120).all(|x| pixel(x, y) == [255, 255, 255])),
            "nothing paints past the clipped row"
        );
    }

    #[test]
    fn ceri_invokes_allowed_commands_rejects_denied_ones_and_hears_host_events() {
        use crate::bridge::{
            Capabilities, CommandCall, CommandError, CommandReply, CommandRouter, Grant,
        };

        #[derive(Default)]
        struct Router {
            calls: RefCell<Vec<CommandCall>>,
            later: RefCell<Option<CommandReply>>,
            events: RefCell<Vec<String>>,
        }

        impl CommandRouter for Router {
            fn invoke(&self, call: CommandCall, reply: CommandReply) {
                match call.command.as_str() {
                    "greet" => reply.send(Ok(format!("{{\"echo\":{}}}", call.payload))),
                    "slow" => *self.later.borrow_mut() = Some(reply),
                    other => reply.send(Err(CommandError::unknown(other))),
                }
                self.calls.borrow_mut().push(call);
            }

            fn event(&self, window: &str, name: &str, payload: &str) {
                self.events
                    .borrow_mut()
                    .push(format!("{window}:{name}:{payload}"));
            }
        }

        let router = Rc::new(Router::default());
        let capabilities = Capabilities::new(vec![Grant {
            windows: vec!["main".to_string()],
            origins: vec![APP_ORIGIN.to_string()],
            commands: vec![
                "greet".to_string(),
                "slow".to_string(),
                "missing".to_string(),
            ],
        }]);
        let mut browser = Browser::new(bundle(
            r##"<p id="status"></p><button id="stop">stop</button>
            <script>
                const log = (line) => { document.getElementById("status").textContent += line + "|"; };
                log(window.ceri.windowLabel + ":" + (window.ceri === window.ceri));
                window.ceri.invoke("greet", { name: "ceri" }).then((reply) => log("greet:" + reply.echo.name));
                window.ceri.invoke("slow").then((reply) => log("slow:" + reply));
                window.ceri.invoke("delete").catch((error) => log(error.name + ":" + error.code));
                window.ceri.invoke("missing").catch((error) => log("missing:" + error.code));
                window.ceri.listen("tick", (event) => log("tick:" + event.payload.n)).then((unlisten) => document.getElementById("stop").addEventListener("click", unlisten));
                window.ceri.emit("ready", [1]);
            </script>"##,
        ))
        .with_host_bridge(crate::script::HostBridge::new(
            router.clone(),
            capabilities,
            "main",
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let status = |browser: &Browser| {
            let document = browser.document().expect("document");
            let document = document.borrow();
            let status = document.find_element_by_id("status").expect("status");
            document.element_text_content(status).unwrap_or_default()
        };
        assert_eq!(
            status(&browser),
            "main:true|greet:ceri|CeriBridgeError:denied|missing:unknown-command|"
        );
        let calls = router.calls.borrow().clone();
        assert_eq!(calls.len(), 3, "a denied command never reaches the router");
        assert_eq!(calls[0].origin, APP_ORIGIN);
        assert_eq!(calls[0].payload, "{\"name\":\"ceri\"}");
        assert_eq!(calls[1].payload, "{}");
        assert_eq!(*router.events.borrow(), ["main:ready:[1]"]);
        router
            .later
            .borrow_mut()
            .take()
            .expect("the slow reply is held")
            .send(Ok("42".to_string()));
        assert_eq!(browser.pump().expect("pump"), 1);
        assert_eq!(browser.emit_event("tick", "{\"n\":7}").expect("emit"), 1);
        assert!(status(&browser).ends_with("slow:42|tick:7|"));
        browser.click_id("stop").expect("unlisten runs");
        assert_eq!(browser.emit_event("tick", "{\"n\":8}").expect("emit"), 0);
    }

    #[test]
    fn the_sdk_wraps_invoke_results_and_host_errors() {
        use crate::bridge::{Capabilities, CommandCall, CommandReply, CommandRouter, Grant};

        struct Echo;

        impl CommandRouter for Echo {
            fn invoke(&self, call: CommandCall, reply: CommandReply) {
                reply.send(Ok(call.payload));
            }
        }

        let mut resources = bundle(
            r##"<p id="status"></p>
            <script type="module">
                import { invoke, windowLabel, CeriBridgeError } from "/sdk/index.js";
                const status = document.getElementById("status");
                const echoed = await invoke("echo", { n: 3 });
                let denied = "none";
                try {
                    await invoke("secret");
                } catch (error) {
                    denied = (error instanceof CeriBridgeError) + ":" + error.code;
                }
                status.textContent = windowLabel() + "," + echoed.n + "," + denied;
            </script>"##,
        );
        let sdk = std::fs::read(format!("{}/../sdk/index.js", env!("CARGO_MANIFEST_DIR")))
            .expect("the sdk should exist");
        resources
            .insert("/sdk/index.js", "text/javascript", sdk)
            .expect("sdk should be accepted");
        let mut browser = Browser::new(resources).with_host_bridge(crate::script::HostBridge::new(
            Rc::new(Echo),
            Capabilities::new(vec![Grant {
                windows: vec!["*".to_string()],
                origins: vec![APP_ORIGIN.to_string()],
                commands: vec!["echo".to_string()],
            }]),
            "settings",
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        browser.pump().expect("pump");
        let document = browser.document().expect("document");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some("settings,3,true:denied")
        );
    }

    #[test]
    fn stylesheets_import_layer_nest_and_query_media_supports_and_containers() {
        let mut resources = bundle(
            r##"<link rel="stylesheet" href="/main.css">
            <style>
                @layer base, theme;
                @layer theme { #layered { color: rgb(0, 0, 255) } }
                @layer base { #layered { color: rgb(255, 0, 0) } #important { color: rgb(255, 0, 0) !important } }
                #important { color: rgb(0, 128, 0) !important }
                #unlayered { color: rgb(0, 128, 0) }
                @layer theme { #unlayered { color: rgb(255, 0, 0) } }
                @media screen and (min-width: 100px) { @media (orientation: landscape) { #media { color: rgb(0, 128, 0) } } }
                @media print, (max-width: 50px) { #media2 { color: rgb(255, 0, 0) } }
                @media not print and (width >= 300px) { #media2 { width: 10px } }
                @supports (display: grid) and (not (display: contents)) { #supports { color: rgb(0, 128, 0) } }
                @supports selector(:has(a)) or (display: nonsense) { #supports { width: 20px } }
                .card { color: rgb(255, 0, 0); & .title { color: rgb(0, 128, 0) } &.wide { width: 30px } @media (min-width: 1px) { background-color: rgb(0, 0, 255) } }
                #calc { width: calc((100px - 2 * 10px) / 4 + 1px) }
                #strings::after { content: "a;b{}" }
                #strings { background-image: url(data:image/png;base64,AAAA); color: rgb(0, 128, 0) }
                #comment::before { content: "/* kept */" }
                #comment { color: rgb(0, 128, 0) }
                .box { container-type: inline-size; width: 300px }
                @container (min-width: 200px) { .inner { color: rgb(0, 128, 0) } }
                @container (min-width: 400px) { .inner { color: rgb(255, 0, 0) } }
            </style>
            <div id="layered">a</div><div id="important">a</div><div id="unlayered">a</div>
            <div id="media">a</div><div id="media2">a</div><div id="supports">a</div>
            <div class="card wide" id="card"><span class="title" id="title">t</span></div>
            <div id="calc">a</div><div id="strings">a</div><div id="comment">a</div>
            <div class="box"><div class="inner" id="inner">a</div></div>
            <div id="imported">a</div><div id="main">a</div>
            <p id="status">idle</p>
            <script>
                const style = (id) => getComputedStyle(document.getElementById(id));
                document.getElementById("status").textContent = [
                    style("layered").color, style("important").color, style("unlayered").color,
                    style("media").color, style("media2").color, style("media2").width,
                    style("supports").color, style("supports").width,
                    style("title").color, style("card").width, style("card").backgroundColor,
                    style("calc").width, style("strings").color, style("comment").color,
                    style("inner").color, style("imported").color, style("main").color,
                ].join("|");
            </script>"##,
        );
        resources
            .insert(
                "/main.css",
                "text/css",
                b"@charset \"utf-8\";\n@import url(\"/imported.css\") screen;\n#main { color: rgb(0, 128, 0) }"
                    .to_vec(),
            )
            .expect("main stylesheet should be accepted");
        resources
            .insert(
                "/imported.css",
                "text/css",
                b"#imported { color: rgb(0, 128, 0) }".to_vec(),
            )
            .expect("imported stylesheet should be accepted");
        let mut browser = Browser::new(resources);
        browser.set_viewport(
            Size {
                width: 600.0,
                height: 400.0,
            },
            1.0,
        );
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let layout = browser
            .layout(Size {
                width: 600.0,
                height: 400.0,
            })
            .expect("layout should build");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let inner = document.find_element_by_id("inner").expect("inner");
        // the page's own layout answers the container query too
        assert_eq!(
            find_node_box(&layout.root, inner).map(|inner| inner.text[0].color),
            Some(Color::opaque(0, 128, 0))
        );
        let status = document.find_element_by_id("status").expect("status");
        let green = "rgb(0, 128, 0)";
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some(
                [
                    "rgb(0, 0, 255)",
                    "rgb(255, 0, 0)",
                    green,
                    green,
                    "rgb(0, 0, 0)",
                    "10px",
                    green,
                    "20px",
                    green,
                    "30px",
                    "rgb(0, 0, 255)",
                    "21px",
                    green,
                    green,
                    green,
                    green,
                    green,
                ]
                .join("|")
                .as_str()
            )
        );
    }

    #[test]
    fn tables_size_columns_span_cells_align_content_and_number_list_items() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                table { border-spacing: 4px; font-size: 16px; line-height: 20px }
                td { padding: 0; height: 20px }
                .wide { width: 100px }
                #tall { height: 60px; vertical-align: bottom }
                #fixed { table-layout: fixed; width: 210px; border-spacing: 0 }
                ol { margin: 0; padding-left: 40px; font-size: 16px; line-height: 20px }
            </style>
            <table id="table">
              <tr id="first"><td id="a" class="wide"></td><td id="b" style="width: 50px"></td></tr>
              <tr><td id="span" colspan="2"></td></tr>
              <tr><td id="down" rowspan="2"></td><td id="tall"><span id="bottom" style="display: inline-block; height: 20px; width: 10px"></span></td></tr>
              <tr><td id="under"></td></tr>
            </table>
            <table id="fixed"><tr><td id="f1" style="width: 10px"></td><td id="f2"></td><td id="f3"></td></tr></table>
            <ol start="4" id="list"><li id="i4">four</li><li>five</li></ol>
            <p id="status">idle</p>
            <script>
                const box = (id) => document.getElementById(id).getBoundingClientRect();
                const x = (id) => box(id).x - box("table").x;
                const y = (id) => box(id).y - box("table").y;
                document.getElementById("status").textContent = [
                    box("table").width, x("a"), x("b"), box("b").width,
                    box("span").width, y("span"), box("down").height, y("bottom"),
                    y("under"), box("first").width,
                    box("f2").width, box("f3").x - box("fixed").x,
                    getComputedStyle(document.getElementById("i4")).display,
                ].join(",");
            </script>"##,
        ));
        browser.set_viewport(
            Size {
                width: 400.0,
                height: 600.0,
            },
            1.0,
        );
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("table page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some("162,4,108,50,154,28,84,92,116,154,100,110,list-item")
        );
        let layout = browser
            .layout(Size {
                width: 400.0,
                height: 600.0,
            })
            .expect("layout should build");
        let marker = find_marker(&layout.root).expect("the list item has a marker");
        assert_eq!(marker.text[0].text, "4.");
    }

    fn find_node_box(
        layout: &crate::layout::LayoutBox,
        node: NodeId,
    ) -> Option<&crate::layout::LayoutBox> {
        if layout.node == node {
            return Some(layout);
        }
        layout
            .children
            .iter()
            .find_map(|child| find_node_box(child, node))
    }

    fn find_marker(layout: &crate::layout::LayoutBox) -> Option<&crate::layout::LayoutBox> {
        if layout.tag == "::marker" {
            return Some(layout);
        }
        layout.children.iter().find_map(find_marker)
    }

    #[test]
    fn inline_elements_get_a_box_per_line_that_paints_and_takes_hits() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                body { margin: 0; width: 200px; font-size: 16px; line-height: 20px }
                #span { padding: 0 4px; border-left: 2px solid #000000; background: #ffcc00 }
            </style>
            <p style="margin: 0">x <span id="span">one two three four five six seven</span> <a id="link" href="#next">go</a></p>
            <p id="status">idle</p>
            <script>
                const span = document.getElementById("span");
                const rects = span.getClientRects();
                const first = rects[0];
                document.getElementById("status").textContent = [
                    rects.length > 1, first.height, rects[rects.length - 1].y > first.y,
                ].join(",");
            </script>"##,
        ));
        let viewport = Size {
            width: 400.0,
            height: 300.0,
        };
        browser.set_viewport(viewport, 1.0);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some("true,16,true")
        );
        let layout = browser.layout(viewport).expect("layout should build");
        let link = document.find_element_by_id("link").expect("link");
        let span = document.find_element_by_id("span").expect("span");
        let link_box = find_node_box(&layout.root, link).expect("the link has a box");
        assert_eq!(
            layout.hit_test(link_box.rect.x + 2.0, link_box.rect.y + 5.0),
            Some(link)
        );
        let span_box = find_node_box(&layout.root, span).expect("the span has a box");
        assert_eq!(span_box.background, Color::opaque(255, 204, 0));
        assert_eq!(span_box.border.widths.left, 2.0);
    }

    #[test]
    fn floats_stack_to_their_sides_push_lines_aside_and_clear() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                #wrap { width: 300px; font-size: 16px; line-height: 20px }
                .left { float: left; width: 100px; height: 50px }
                .right { float: right; width: 80px; height: 30px }
                #second { width: 150px }
                #clear { clear: left; height: 10px }
                #context { overflow: hidden }
                #inner { float: left; width: 40px; height: 70px }
            </style>
            <div id="wrap">
            <div class="left" id="first"></div><div class="right" id="right"></div>
            <div class="left" id="second"></div>
            <p id="text" style="margin: 0"><span id="word" style="display: inline-block">word</span> <span id="plain">more</span></p>
            <div id="clear"></div>
            <div id="context"><div id="inner"></div><span id="beside" style="display: inline-block">x</span></div>
            </div>
            <p id="status">idle</p>
            <script>
                const box = (id) => document.getElementById(id).getBoundingClientRect();
                document.getElementById("status").textContent = [
                    box("first").x, box("right").x, box("second").x, box("second").y,
                    box("word").x, box("word").y,
                    box("clear").y,
                    box("beside").x, box("context").height,
                    box("plain").x >= box("word").x + box("word").width,
                    box("plain").height > 10, box("plain").y,
                    document.getElementById("plain").getClientRects().length,
                ].join(",");
            </script>"##,
        ));
        browser.set_viewport(
            Size {
                width: 400.0,
                height: 600.0,
            },
            1.0,
        );
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("float page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some("0,220,100,30,100,0,80,40,70,true,true,2,1")
        );
    }

    #[test]
    fn overflow_axes_visibility_rounded_hits_and_cursors_follow_style() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                body { margin: 0 }
                #axes { width: 50px; height: 20px; overflow-x: hidden }
                #clip { width: 50px; height: 20px; overflow: clip visible }
                #hidden { visibility: hidden; width: 50px; height: 50px }
                #shown { visibility: visible; width: 20px; height: 20px }
                #link { display: block; width: 50px; height: 20px }
                #round { width: 40px; height: 40px; border-radius: 20px }
                #move { cursor: url(grab.png) 2 2, move; width: 50px; height: 20px }
                #text { height: 20px; font-size: 14px }
            </style>
            <div id="axes"></div><div id="clip"></div>
            <div id="hidden"><div id="shown"></div></div>
            <a id="link" href="#next">go</a><div id="round"></div><div id="move"></div>
            <div id="text">hi</div>
            <p id="status">idle</p>
            <script>
                const style = (id) => getComputedStyle(document.getElementById(id));
                document.getElementById("status").textContent = [
                    style("axes").overflowX, style("axes").overflowY,
                    style("clip").overflowX, style("clip").overflowY,
                ].join(",");
            </script>"##,
        ));
        let viewport = Size {
            width: 400.0,
            height: 600.0,
        };
        browser.set_viewport(viewport, 1.0);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let id = |name: &str| document.find_element_by_id(name).expect("element exists");
        assert_eq!(
            document.element_text_content(id("status")).as_deref(),
            Some("hidden,auto,clip,visible")
        );
        let layout = browser.layout(viewport).expect("layout should build");
        assert_eq!(layout.hit_test(10.0, 50.0), Some(id("shown")));
        assert_ne!(
            layout.hit_test(40.0, 80.0),
            Some(id("hidden")),
            "a hidden box is not a hit target"
        );
        assert_eq!(layout.hit_test(20.0, 130.0), Some(id("round")));
        assert_ne!(
            layout.hit_test(1.0, 111.0),
            Some(id("round")),
            "points outside a rounded corner miss the box"
        );
        assert_eq!(layout.cursor_at(10.0, 100.0), Cursor::Pointer);
        assert_eq!(layout.cursor_at(10.0, 160.0), Cursor::Move);
        assert_eq!(layout.cursor_at(3.0, 178.0), Cursor::Text);
        assert_eq!(layout.cursor_at(300.0, 178.0), Cursor::Default);
    }

    #[test]
    fn browser_scrolls_auto_overflow_and_dispatches_scroll_events() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                #viewport { width: 40px; height: 20px; overflow: auto; margin: 0; }
                #content { width: 80px; height: 60px; margin: 0; background: #ff0000; }
            </style>
            <div id="viewport"><div id="content"></div></div>
            <p id="status">idle</p>
            <script>
                const viewport = document.getElementById("viewport");
                const status = document.getElementById("status");
                viewport.addEventListener("scroll", () => {
                    status.textContent = "scroll:" + viewport.scrollTop + ":" + viewport.scrollLeft;
                });
            </script>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        assert!(
            browser
                .scroll_at(
                    Size {
                        width: 120.0,
                        height: 100.0,
                    },
                    10.0,
                    10.0,
                    0.0,
                    30.0,
                )
                .expect("wheel scroll should dispatch")
        );
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("scroll:30:0")
        );
        let layout = browser
            .layout(Size {
                width: 120.0,
                height: 100.0,
            })
            .expect("scrolled page should layout");
        assert_eq!(layout.body().children[0].children[0].rect.y, -30.0);
        assert!(
            browser
                .scroll_at(
                    Size {
                        width: 120.0,
                        height: 100.0,
                    },
                    10.0,
                    10.0,
                    0.0,
                    100.0,
                )
                .expect("scroll should clamp to the content boundary")
        );
        assert!(
            !browser
                .scroll_at(
                    Size {
                        width: 120.0,
                        height: 100.0,
                    },
                    10.0,
                    10.0,
                    0.0,
                    100.0,
                )
                .expect("scroll at the boundary should be ignored")
        );
    }

    #[test]
    fn browser_keeps_sticky_content_at_the_scrollport_edge() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                #viewport { width: 80px; height: 40px; overflow: auto; margin: 0; }
                #sticky { position: sticky; top: 0; width: 80px; height: 10px; margin: 0; background: #00ff00; }
                #spacer { width: 80px; height: 80px; margin: 0; background: #ff0000; }
            </style>
            <div id="viewport"><div id="sticky">sticky</div><div id="spacer"></div></div>
            <p id="status">idle</p>
            <script>
                document.getElementById("sticky").addEventListener("click", () => {
                    document.getElementById("status").textContent = "sticky";
                });
            </script>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("sticky page should load");
        let viewport = Size {
            width: 120.0,
            height: 80.0,
        };
        assert!(
            browser
                .scroll_at(viewport, 10.0, 10.0, 0.0, 20.0)
                .expect("sticky wheel input should scroll")
        );
        let layout = browser.layout(viewport).expect("sticky page should layout");
        let sticky = &layout.body().children[0].children[0];
        assert_eq!(sticky.rect.y, -20.0);
        assert_eq!(sticky.visual_offset, (0.0, 20.0));
        let canvas = browser.paint(viewport).expect("sticky page should paint");
        let pixel = &canvas.rgba_pixels()[(4 * canvas.width() as usize + 10) * 4..];
        assert_eq!(&pixel[..4], &[0, 255, 0, 255]);
        assert!(
            browser
                .click_at(viewport, 10.0, 4.0)
                .expect("sticky click should dispatch")
        );
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("sticky")
        );
    }

    #[test]
    fn browser_keeps_sticky_document_content_at_the_viewport_edge() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                #sticky { position: sticky; top: 0; width: 120px; height: 10px; margin: 0; background: #00ff00; }
                #spacer { width: 120px; height: 160px; margin: 0; background: #ff0000; }
            </style>
            <div id="sticky">sticky</div><div id="spacer"></div>
            <p id="status">idle</p>
            <script>
                document.getElementById("sticky").addEventListener("click", () => {
                    document.getElementById("status").textContent = "sticky";
                });
            </script>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("sticky page should load");
        let viewport = Size {
            width: 120.0,
            height: 80.0,
        };
        assert!(
            browser
                .scroll_at(viewport, 10.0, 70.0, 0.0, 20.0)
                .expect("sticky wheel input should scroll")
        );
        let layout = browser.layout(viewport).expect("sticky page should layout");
        let sticky = &layout.body().children[0];
        assert_eq!(sticky.rect.y, -20.0);
        assert_eq!(sticky.visual_offset, (0.0, 20.0));
        let canvas = browser.paint(viewport).expect("sticky page should paint");
        let pixel = &canvas.rgba_pixels()[(4 * canvas.width() as usize + 10) * 4..];
        assert_eq!(&pixel[..4], &[0, 255, 0, 255]);
        assert!(
            browser
                .click_at(viewport, 10.0, 4.0)
                .expect("sticky click should dispatch")
        );
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("sticky")
        );
    }

    #[test]
    fn browser_applies_inherited_custom_properties_to_layout() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                :root { --accent: #00ff00; --card-width: calc(50% - 10px); margin: 0; }
                #card { width: var(--card-width); height: calc(10px + 10px); margin: 0; background: var(--accent); }
            </style>
            <html><div id="card">card</div></html>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("custom-property page should load");
        let viewport = Size {
            width: 100.0,
            height: 60.0,
        };
        let layout = browser
            .layout(viewport)
            .expect("custom-property page should layout");
        let card = &layout.body().children[0];
        assert_eq!(card.rect.width, 40.0);
        assert_eq!(card.rect.height, 20.0);
        let canvas = browser
            .paint(viewport)
            .expect("custom-property page should paint");
        let pixel = &canvas.rgba_pixels()[(4 * canvas.width() as usize + 4) * 4..];
        assert_eq!(&pixel[..4], &[0, 255, 0, 255]);
    }

    #[test]
    fn browser_paints_scrollbars_and_scrolls_from_the_reserved_gutter() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                #viewport { width: 80px; height: 80px; overflow: auto; margin: 0; }
                #content { width: 160px; height: 120px; margin: 0; background: #ff0000; }
            </style>
            <div id="viewport"><div id="content"></div></div>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("scrollbar page should load");
        let viewport = Size {
            width: 120.0,
            height: 120.0,
        };
        let canvas = browser
            .paint(viewport)
            .expect("scrollbar page should paint");
        let content = ((4 * canvas.width() + 10) * 4) as usize;
        let vertical_thumb = ((25 * canvas.width() + 72) * 4) as usize;
        let horizontal_thumb = ((72 * canvas.width() + 24) * 4) as usize;
        let vertical_track = ((40 * canvas.width() + 66) * 4) as usize;
        let horizontal_track = ((66 * canvas.width() + 40) * 4) as usize;
        assert_eq!(
            &canvas.rgba_pixels()[content..content + 4],
            [255, 0, 0, 255]
        );
        assert_eq!(
            &canvas.rgba_pixels()[vertical_thumb..vertical_thumb + 4],
            [139, 139, 139, 255]
        );
        assert_eq!(
            &canvas.rgba_pixels()[horizontal_thumb..horizontal_thumb + 4],
            [139, 139, 139, 255]
        );
        assert_eq!(
            &canvas.rgba_pixels()[vertical_track..vertical_track + 4],
            [252, 252, 252, 255]
        );
        assert_eq!(
            &canvas.rgba_pixels()[horizontal_track..horizontal_track + 4],
            [252, 252, 252, 255]
        );
        assert!(
            browser
                .scroll_at(viewport, 72.0, 40.0, 0.0, 10.0)
                .expect("wheel input over the scrollbar should scroll")
        );
        let layout = browser
            .layout(viewport)
            .expect("scrolled scrollbar page should layout");
        assert_eq!(layout.body().children[0].scroll_offset, (0.0, 10.0));
        assert_eq!(layout.body().children[0].children[0].rect.y, -10.0);
    }

    #[test]
    fn browser_scrolls_the_document_and_preserves_fixed_viewport_chrome() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                #content { width: 60px; height: 160px; margin: 0; background: #ff0000; }
                #status { position: fixed; left: 0; top: 0; width: 60px; height: 12px; margin: 0; }
            </style>
            <div id="content"></div>
            <div id="status">idle</div>
            <script>
                const status = document.getElementById("status");
                window.addEventListener("scroll", () => {
                    status.textContent = "window:" + window.scrollY + ":" + window.pageYOffset;
                });
            </script>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let viewport = Size {
            width: 120.0,
            height: 100.0,
        };
        let canvas = browser
            .paint(viewport)
            .expect("document scrollbar page should paint");
        let vertical_track = ((80 * canvas.width() + 115) * 4) as usize;
        let vertical_thumb = ((20 * canvas.width() + 115) * 4) as usize;
        assert_eq!(
            &canvas.rgba_pixels()[vertical_track..vertical_track + 4],
            [252, 252, 252, 255]
        );
        assert_eq!(
            &canvas.rgba_pixels()[vertical_thumb..vertical_thumb + 4],
            [139, 139, 139, 255]
        );
        assert!(
            browser
                .scroll_at(viewport, 10.0, 10.0, 0.0, 30.0)
                .expect("document wheel scroll should dispatch")
        );
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("window:30:30")
        );
        let layout = browser
            .layout(viewport)
            .expect("scrolled document should layout");
        assert_eq!(layout.body().children[0].rect.y, -30.0);
        assert_eq!(layout.body().children[1].rect.y, 0.0);
        assert!(
            browser
                .scroll_at(viewport, 10.0, 10.0, 0.0, 100.0)
                .expect("document scroll should clamp to the page boundary")
        );
        assert!(
            !browser
                .scroll_at(viewport, 10.0, 10.0, 0.0, 100.0)
                .expect("document scroll at the boundary should be ignored")
        );
    }

    #[test]
    fn browser_advances_page_tasks_deterministically() {
        let mut browser = Browser::new(bundle(
            r#"<p id="status">waiting</p><script>setTimeout(() => { document.getElementById("status").textContent = "ready"; }, 40);</script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        assert_eq!(browser.advance_time(39).expect("clock should advance"), 0);
        assert_eq!(browser.advance_time(1).expect("timer should run"), 1);
        let document = browser.document().unwrap();
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status element should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("ready")
        );
    }

    #[test]
    fn browser_runs_external_dom_construction_before_layout() {
        let mut resources = bundle(
            r#"<main id="mount"><p>before</p></main><script src="/assets/build.js"></script>"#,
        );
        resources
            .insert(
                "/assets/build.js",
                "text/javascript",
                br#"const item = document.createElement("p"); item.className = "created"; item.textContent = "after"; document.getElementById("mount").appendChild(item);"#.to_vec(),
            )
            .expect("script should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let document = browser.document().unwrap();
        let created = document
            .borrow()
            .find_element_by_selector(".created")
            .expect("created element should exist");
        assert_eq!(
            document.borrow().element_text_content(created).as_deref(),
            Some("after")
        );
    }

    #[test]
    fn loads_external_styles_and_scripts_in_document_order() {
        let mut resources = bundle(
            r#"<link rel="stylesheet" href="/assets/theme.css"><p id="status">idle</p><script src="/assets/boot.js"></script><script>document.getElementById("status").textContent = message;</script>"#,
        );
        resources
            .insert(
                "/assets/theme.css",
                "text/css",
                b"p { color: #42d3b4; }".to_vec(),
            )
            .expect("stylesheet should be accepted");
        resources
            .insert(
                "/assets/boot.js",
                "text/javascript; charset=utf-8",
                b"const message = \"external script ready\";".to_vec(),
            )
            .expect("script should be accepted");
        let mut browser = Browser::new(resources);
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("page should load"),
            NavigationStatus::Loaded
        );
        let document = browser.document().unwrap();
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status element should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("external script ready")
        );
        let layout = browser
            .layout(Size {
                width: 320.0,
                height: 120.0,
            })
            .expect("page should layout");
        assert_eq!(
            layout.body().children[0].text[0].color,
            crate::Color::opaque(66, 211, 180)
        );
    }

    #[test]
    fn loads_the_document_and_all_subresources_from_a_host_provider() {
        let mut provided = BTreeMap::new();
        provided.insert(
            "/index.html".to_string(),
            Resource::new(
                "text/html; charset=utf-8",
                br#"<link rel="stylesheet" href="/theme.css"><p id="status">idle</p><script type="module" src="/entry.js"></script>"#.to_vec(),
            ),
        );
        provided.insert(
            "/theme.css".to_string(),
            Resource::new("text/css", b"p { color: #42d3b4; }".to_vec()),
        );
        provided.insert(
            "/entry.js".to_string(),
            Resource::new(
                "text/javascript",
                br#"import { label } from "./message.js"; fetch("/data.json").then((response) => response.json()).then((data) => { document.getElementById("status").textContent = label + "|" + data.message; });"#.to_vec(),
            ),
        );
        provided.insert(
            "/message.js".to_string(),
            Resource::new(
                "text/javascript",
                br#"export const label = "provider";"#.to_vec(),
            ),
        );
        provided.insert(
            "/data.json".to_string(),
            Resource::new("application/json", br#"{"message":"ready"}"#.to_vec()),
        );

        let provider = Rc::new(MemoryProvider {
            resources: provided,
        });
        let mut browser = Browser::with_resource_provider(ResourceBundle::default(), provider);
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("provider-backed page should load"),
            NavigationStatus::Loaded
        );
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status element should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("provider|ready")
        );
        let layout = browser
            .layout(Size {
                width: 320.0,
                height: 120.0,
            })
            .expect("provider-backed page should layout");
        assert_eq!(
            layout.body().children[0].text[0].color,
            Color::opaque(66, 211, 180)
        );
    }

    #[test]
    fn forwards_fetch_request_metadata_to_the_host_provider() {
        let requests = Rc::new(RefCell::new(Vec::new()));
        let mut provided = BTreeMap::new();
        provided.insert(
            "/index.html".to_string(),
            Resource::new(
                "text/html",
                br#"<p id="status">idle</p><script>fetch("/api", { method: "post", headers: { "X-Trace": "lab", "Content-Type": "application/json" }, body: "{\"name\":\"ceri\"}", cache: "no-store" }).then((response) => response.text().then((body) => { document.getElementById("status").textContent = response.status + "|" + response.headers.get("x-provider") + "|" + body; }));</script>"#.to_vec(),
            ),
        );
        provided.insert(
            "/api".to_string(),
            Resource::new("application/json", br#"{"ok":true}"#.to_vec()),
        );
        let provider = Rc::new(RequestProvider {
            resources: provided,
            requests: requests.clone(),
        });
        let mut browser = Browser::with_resource_provider(ResourceBundle::default(), provider);
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("request-backed page should load"),
            NavigationStatus::Loaded
        );
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status element should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("201|yes|{\"ok\":true}")
        );
        let request = requests
            .borrow()
            .iter()
            .find(|request| request.method() == "POST")
            .cloned()
            .expect("provider should receive the fetch request");
        assert_eq!(request.method(), "POST");
        assert_eq!(request.headers().get("x-trace"), Some(&"lab".to_string()));
        assert_eq!(
            request.headers().get("content-type"),
            Some(&"application/json".to_string())
        );
        assert_eq!(request.cache_mode(), &ResourceCacheMode::NoStore);
        assert_eq!(request.body(), br#"{"name":"ceri"}"#);
    }

    #[test]
    fn packaged_static_modules_execute_with_same_origin_imports() {
        let mut resources =
            bundle(r#"<p id="status">idle</p><script type="module" src="/entry.js"></script>"#);
        resources
            .insert(
                "/entry.js",
                "text/javascript",
                br#"import { message, suffix as addSuffix } from "./message.js"; import * as messages from "./message.js"; document.getElementById("status").textContent = addSuffix(messages.message) + "|" + message;"#.to_vec(),
            )
            .expect("entry module should be accepted");
        resources
            .insert(
                "/message.js",
                "text/javascript",
                br#"export const message = "from module"; export function suffix(value) { return value + " ready"; }"#.to_vec(),
            )
            .expect("dependency module should be accepted");
        let mut browser = Browser::new(resources);
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("module page should load"),
            NavigationStatus::Loaded
        );
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("from module ready|from module")
        );
    }

    #[test]
    fn cross_origin_module_imports_install_the_recovery_document() {
        let mut resources =
            bundle(r#"<p>before</p><script type="module" src="/entry.js"></script>"#);
        resources
            .insert(
                "/entry.js",
                "text/javascript",
                br#"import "https://outside.invalid/module.js";"#.to_vec(),
            )
            .expect("entry module should be accepted");
        let mut browser = Browser::new(resources);
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("module failure should become recovery"),
            NavigationStatus::ErrorPage { status: 500 }
        );
        let document = browser.document().expect("recovery document should exist");
        let recovery = document
            .borrow()
            .find_element_by_id("recovery")
            .expect("recovery element should exist");
        assert_eq!(
            document
                .borrow()
                .attribute(recovery, "data-status")
                .map(str::to_string),
            Some("500".to_string())
        );
    }

    #[test]
    fn page_scripts_fetch_same_origin_packaged_text() {
        let mut resources = bundle(
            r#"<p id="status">idle</p><script>const status = document.getElementById("status"); fetch("/data.json").then((response) => { response.text().then((body) => { status.textContent = response.status + " " + body; }); });</script>"#,
        );
        resources
            .insert(
                "/data.json",
                "application/json",
                br#"{"message":"from bundle"}"#.to_vec(),
            )
            .expect("data resource should be accepted");
        let mut browser = Browser::new(resources);
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("page should load"),
            NavigationStatus::Loaded
        );
        let document = browser.document().unwrap();
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status element should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("200 {\"message\":\"from bundle\"}")
        );
    }

    #[test]
    fn missing_external_resources_use_the_recovery_document() {
        let mut browser = Browser::new(bundle(
            r#"<link rel="stylesheet" href="/assets/missing.css"><p>home</p>"#,
        ));
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("missing subresource should become recovery page"),
            NavigationStatus::ErrorPage { status: 500 }
        );
        let document = browser.document().unwrap();
        let recovery = document
            .borrow()
            .find_element_by_id("recovery")
            .expect("recovery element should exist");
        assert_eq!(
            document
                .borrow()
                .attribute(recovery, "data-status")
                .map(str::to_string),
            Some("500".to_string())
        );
    }

    #[test]
    fn navigation_renders_through_the_custom_paint_path() {
        let mut browser = Browser::new(bundle(
            "<style>body { background: #123456; }</style><p>painted</p>",
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let canvas = browser
            .paint(Size {
                width: 240.0,
                height: 120.0,
            })
            .expect("page should paint");
        assert!(canvas.non_background_pixels() > 0);
    }

    #[test]
    fn media_queries_follow_the_requested_viewport() {
        let mut browser = Browser::new(bundle(
            "<style>p { color: red; } @media screen and (min-width: 500px) { p { color: blue; } } @media (max-height: 100px) { p { color: #42d3b4; } }</style><p>responsive</p>",
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("responsive page should load");
        let narrow = browser
            .layout(Size {
                width: 400.0,
                height: 200.0,
            })
            .expect("narrow layout should build");
        assert_eq!(
            narrow.body().children[0].text[0].color,
            Color::opaque(255, 0, 0)
        );
        let wide = browser
            .layout(Size {
                width: 600.0,
                height: 200.0,
            })
            .expect("wide layout should build");
        assert_eq!(
            wide.body().children[0].text[0].color,
            Color::opaque(0, 0, 255)
        );
        let short = browser
            .layout(Size {
                width: 400.0,
                height: 80.0,
            })
            .expect("short layout should build");
        assert_eq!(
            short.body().children[0].text[0].color,
            Color::opaque(66, 211, 180)
        );
    }

    #[test]
    fn canvas_commands_reach_the_custom_paint_path() {
        let mut browser = Browser::new(bundle(
            r##"<canvas id="art" style="display: block; width: 80px; height: 60px"></canvas><script>
                const ctx = document.getElementById("art").getContext("2d");
                ctx.fillStyle = "#ff0000";
                ctx.fillRect(4, 5, 10, 8);
            </script>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("canvas page should load");
        let canvas = browser
            .paint(Size {
                width: 160.0,
                height: 120.0,
            })
            .expect("canvas page should paint");
        assert!(
            canvas
                .rgba_pixels()
                .chunks_exact(4)
                .any(|pixel| pixel == [255, 0, 0, 255])
        );
    }

    #[test]
    fn packaged_svg_images_reach_the_custom_paint_path() {
        let mut resources = bundle(
            r#"<img id="icon" src="/icon.svg" style="display: block; width: 80px; height: 48px">"#,
        );
        resources
            .insert(
                "/icon.svg",
                "image/svg+xml",
                br##"<svg width="16" height="10" viewbox="0 0 16 10"><rect width="16" height="10" fill="#07131f"/><circle cx="5" cy="5" r="3" fill="#6ee7c5"/><rect x="10" y="2" width="4" height="6" fill="#e06c75"/></svg>"##.to_vec(),
            )
            .expect("svg resource should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("image page should load");
        let layout = browser
            .layout(Size {
                width: 120.0,
                height: 80.0,
            })
            .expect("image layout should build");
        assert_eq!(layout.images.len(), 1);
        let canvas = browser
            .paint(Size {
                width: 120.0,
                height: 80.0,
            })
            .expect("image page should paint");
        assert!(
            canvas
                .rgba_pixels()
                .chunks_exact(4)
                .any(|pixel| pixel == [110, 231, 197, 255])
        );
        assert!(
            canvas
                .rgba_pixels()
                .chunks_exact(4)
                .any(|pixel| pixel == [224, 108, 117, 255])
        );
    }

    #[test]
    fn inline_svgs_size_from_attributes_and_view_boxes_and_take_css_paint() {
        let mut browser = Browser::new(bundle(
            r##"<style>.styled path { fill: #0000ff }</style><div style="width: 200px"><svg width="24" height="24" viewBox="0 0 12 12" style="display: block; color: #ff0000"><path d="M0 0H12V12H0Z" fill="currentColor"/></svg><svg class="styled" width="24" height="24" style="display: block"><path d="M0 0h24v24h-24z" fill="#00ff00"/></svg><svg viewBox="0 0 4 2" style="display: block"></svg><svg style="display: block"></svg></div>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("svg page should load");
        let viewport = Size {
            width: 320.0,
            height: 360.0,
        };
        let layout = browser.layout(viewport).expect("svg layout should build");
        let block = find_box(&layout.root, "div").expect("div");
        let sizes: Vec<_> = block
            .children
            .iter()
            .map(|child| (child.rect.width, child.rect.height))
            .collect();
        assert_eq!(
            sizes,
            [(24.0, 24.0), (24.0, 24.0), (200.0, 100.0), (300.0, 150.0)]
        );
        let canvas = browser.paint(viewport).expect("svg page should paint");
        assert_eq!(pixel_at(&canvas, 12, 12), [255, 0, 0, 255]);
        assert_eq!(pixel_at(&canvas, 12, 36), [0, 0, 255, 255]);
    }

    fn image_bundle(source: &str) -> ResourceBundle {
        let mut resources = bundle(source);
        for name in ["rgb8.png", "rgba8.png", "baseline-420.jpg", "frame.gif"] {
            let bytes = std::fs::read(format!(
                "{}/tests/fixtures/{name}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .expect("image fixture should exist");
            resources
                .insert(&format!("/{name}"), mime_type_for(Path::new(name)), bytes)
                .expect("image fixture should be accepted");
        }
        resources
    }

    fn image_page(source: &str, width: f32, height: f32) -> (Browser, Rc<LayoutDocument>, Canvas) {
        let mut browser = Browser::new(image_bundle(source));
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("page should load"),
            NavigationStatus::Loaded
        );
        let viewport = Size { width, height };
        let layout = browser.layout(viewport).expect("layout should build");
        let canvas = browser.paint(viewport).expect("page should paint");
        (browser, layout, canvas)
    }

    fn find_box<'a>(
        layout: &'a crate::layout::LayoutBox,
        tag: &str,
    ) -> Option<&'a crate::layout::LayoutBox> {
        if layout.tag == tag {
            return Some(layout);
        }
        layout
            .children
            .iter()
            .find_map(|child| find_box(child, tag))
    }

    fn pixel_at(canvas: &Canvas, x: u32, y: u32) -> [u8; 4] {
        let index = ((y * canvas.width() + x) * 4) as usize;
        canvas.rgba_pixels()[index..index + 4]
            .try_into()
            .expect("four channels")
    }

    // the generated fixtures color pixel (x, y) from its coordinates
    fn fixture_color(x: u32, y: u32) -> [u8; 4] {
        [
            ((x * 19 + y * 7) % 256) as u8,
            ((x * 5 + y * 31) % 256) as u8,
            ((x * 11 + y * 13 + 40) % 256) as u8,
            255,
        ]
    }

    #[test]
    fn an_inline_image_takes_its_natural_size_and_sits_on_the_line() {
        let (_, layout, canvas) = image_page(
            "<style>body { margin: 0; } p { margin: 0; font-size: 16px; }</style><p>logo <img src=\"/rgb8.png\"> text</p>",
            200.0,
            60.0,
        );
        let image = find_box(&layout.root, "img").expect("the image should lay out");
        assert_eq!((image.rect.width, image.rect.height), (13.0, 7.0));
        assert!(image.rect.x > 0.0, "the image follows the text before it");
        let paragraph = find_box(&layout.root, "p").expect("paragraph");
        let after = paragraph
            .text
            .iter()
            .find(|run| run.text.trim() == "text")
            .expect("text after the image");
        assert!(after.x >= image.rect.x + image.rect.width);
        let (x, y) = (image.rect.x as u32, image.rect.y as u32);
        assert_eq!(pixel_at(&canvas, x + 2, y + 3), fixture_color(2, 3));
    }

    #[test]
    fn one_declared_side_scales_the_other_by_the_natural_ratio() {
        let (_, layout, _) = image_page(
            "<style>body { margin: 0; }</style><img src=\"/rgb8.png\" style=\"display: block; width: 26px\"><div style=\"width: 30px; aspect-ratio: 3 / 1\"></div>",
            200.0,
            60.0,
        );
        let image = find_box(&layout.root, "img").expect("image");
        assert_eq!((image.rect.width, image.rect.height), (26.0, 14.0));
        let block = find_box(&layout.root, "div").expect("div");
        assert_eq!(block.rect.height, 10.0);
    }

    #[test]
    fn object_fit_contain_letterboxes_and_cover_crops() {
        let (_, _, canvas) = image_page(
            "<style>body { margin: 0; } img { display: block; width: 26px; height: 26px; }</style><img src=\"/rgb8.png\" style=\"object-fit: contain\"><img src=\"/rgb8.png\" style=\"object-fit: cover\">",
            100.0,
            60.0,
        );
        let blank = pixel_at(&canvas, 99, 59);
        // contain scales by two and centers vertically, leaving white bands above and below
        assert_eq!(pixel_at(&canvas, 13, 1), blank);
        assert_ne!(pixel_at(&canvas, 13, 13), blank);
        // cover fills the whole square
        assert_ne!(pixel_at(&canvas, 1, 27), blank);
        assert_ne!(pixel_at(&canvas, 24, 51), blank);
    }

    #[test]
    fn data_urls_decode_without_reaching_the_resource_bundle() {
        let (_, layout, canvas) = image_page(
            "<style>body { margin: 0; }</style><img style=\"display: block\" src=\"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAA0AAAAHCAIAAABcElBNAAAAJHRFWHRDb21tZW50AGFuY2lsbGFyeSBjaHVua3MgYXJlIHNraXBwZWQXJBj8AAAAQElEQVR42mNgYNAQZjVW47Kz5Pf0EQmJl4wvkstqVS6dodGwWrd7n9G0i+YLn9isYWSXNxVm5SaImNjleYlBzHx2erxCwNKhtwAAAEFJREFUPAQRC0g5KzdBxKA8O9NsQYnn0vqYVV3566c2bVkwdefqFfu27T588NyJMw/PXv9y6RH79beMWrvKiPEHAPIZMLzOFGjtAAAAAElFTkSuQmCC\">",
            100.0,
            40.0,
        );
        assert_eq!(layout.images.len(), 1);
        assert_eq!(pixel_at(&canvas, 4, 5), fixture_color(4, 5));
    }

    #[test]
    fn background_images_position_tile_and_respect_no_repeat() {
        let (_, _, canvas) = image_page(
            "<style>body { margin: 0; } div { width: 60px; height: 20px; }</style><div style=\"background: url('/rgb8.png') no-repeat 10px 5px\"></div><div style=\"background-image: url(/rgb8.png)\"></div><div style=\"background: url(/rgb8.png) center / 26px auto no-repeat\"></div>",
            100.0,
            80.0,
        );
        let blank = pixel_at(&canvas, 99, 79);
        assert_eq!(pixel_at(&canvas, 12, 8), fixture_color(2, 3));
        assert_eq!(pixel_at(&canvas, 1, 1), blank);
        assert_eq!(pixel_at(&canvas, 30, 15), blank);
        // the repeating background tiles every 13 by 7 pixels from the box origin
        assert_eq!(pixel_at(&canvas, 15, 20 + 10), fixture_color(2, 3));
        // a sized and centered background leaves the box corners empty
        assert_eq!(pixel_at(&canvas, 1, 41), blank);
        assert_ne!(pixel_at(&canvas, 30, 50), blank);
    }

    #[test]
    fn inline_blocks_shrink_to_their_content_and_flow_with_text() {
        let (_, layout, _) = image_page(
            "<style>body { margin: 0; } p { margin: 0; font-size: 16px; } span.badge { display: inline-block; padding: 2px; background: #ff0000; }</style><p>before <span class=\"badge\">new</span> after</p>",
            300.0,
            60.0,
        );
        let paragraph = find_box(&layout.root, "p").expect("paragraph");
        let badge = paragraph
            .children
            .iter()
            .find(|child| child.tag == "span")
            .expect("the inline-block lays out as a box");
        assert!(
            badge.rect.width < 100.0,
            "an auto width shrinks to the content"
        );
        let before = paragraph
            .text
            .iter()
            .find(|run| run.text.trim() == "before")
            .expect("before");
        let after = paragraph
            .text
            .iter()
            .find(|run| run.text.trim() == "after")
            .expect("after");
        assert!(badge.rect.x >= before.x + before.width);
        assert!(after.x >= badge.rect.x + badge.rect.width);
    }

    #[test]
    fn jpeg_and_gif_images_reach_the_frame() {
        let (_, layout, canvas) = image_page(
            "<style>body { margin: 0; } img { display: block; }</style><img src=\"/baseline-420.jpg\"><img src=\"/frame.gif\">",
            100.0,
            80.0,
        );
        let blank = pixel_at(&canvas, 99, 79);
        assert_eq!(layout.images.len(), 2);
        let jpeg = find_box(&layout.root, "img").expect("jpeg");
        assert_eq!((jpeg.rect.width, jpeg.rect.height), (61.0, 37.0));
        assert_ne!(pixel_at(&canvas, 30, 18), blank);
    }

    fn close(actual: [u8; 4], expected: [u8; 3]) -> bool {
        actual[..3]
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.abs_diff(expected) <= 3)
    }

    #[test]
    fn opacity_fades_a_subtree_as_one_group() {
        let (_, _, canvas) = image_page(
            "<style>body { margin: 0; } #paper { background: #ffffff; width: 100px; height: 100px; }</style><div id=\"paper\"><div style=\"opacity: 0.5; width: 40px; height: 40px; background: #ff0000\"><div style=\"width: 20px; height: 20px; background: #0000ff\"></div></div><div style=\"opacity: 0.5\"><div style=\"opacity: 0.5; width: 10px; height: 10px; background: #ff0000\"></div></div><div style=\"opacity: 0.5; width: 13px; height: 7px\"><img style=\"display: block\" src=\"/rgb8.png\"></div></div>",
            100.0,
            100.0,
        );
        // the child covers the parent inside the group, so no red shows through the blue
        assert!(
            close(pixel_at(&canvas, 10, 10), [128, 128, 255]),
            "{:?}",
            pixel_at(&canvas, 10, 10)
        );
        assert!(
            close(pixel_at(&canvas, 30, 30), [255, 128, 128]),
            "{:?}",
            pixel_at(&canvas, 30, 30)
        );
        // nested groups multiply
        assert!(
            close(pixel_at(&canvas, 5, 45), [255, 191, 191]),
            "{:?}",
            pixel_at(&canvas, 5, 45)
        );
        // images inside a faded group fade with it
        let [red, green, blue, _] = fixture_color(4, 3);
        let mix = |value: u8| ((u16::from(value) + 255) / 2) as u8;
        assert!(
            close(pixel_at(&canvas, 4, 53), [mix(red), mix(green), mix(blue)]),
            "{:?}",
            pixel_at(&canvas, 4, 53)
        );
    }

    #[test]
    fn text_inside_a_faded_box_fades_and_still_takes_clicks() {
        let mut browser = Browser::new(bundle(
            "<style>body { margin: 0; } #paper { background: #ffffff; padding: 4px; } button { display: block; width: 80px; height: 30px; color: #000000; background: #ffffff; font-size: 16px; }</style><div id=\"paper\"><div style=\"opacity: 0.4\"><button id=\"go\">GO</button></div><p id=\"status\">idle</p></div><script>document.getElementById('go').addEventListener('click', () => { document.getElementById('status').textContent = 'clicked'; });</script>",
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let viewport = Size {
            width: 120.0,
            height: 80.0,
        };
        let canvas = browser.paint(viewport).expect("page should paint");
        let darkest = (4..34)
            .flat_map(|y| (4..84).map(move |x| (x, y)))
            .map(|(x, y)| pixel_at(&canvas, x, y)[0])
            .min()
            .expect("button pixels");
        assert!(
            darkest >= 140,
            "black text at 40% opacity stays light, got {darkest}"
        );
        assert!(
            browser
                .click_at(viewport, 20.0, 15.0)
                .expect("click should route")
        );
        let document = browser.document().expect("document");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("clicked")
        );
    }

    #[test]
    fn rotation_moves_pixels_around_the_transform_origin() {
        let (_, _, canvas) = image_page(
            "<style>body { margin: 0; }</style><div style=\"margin: 20px; width: 40px; height: 20px; background: #ff0000; transform: rotate(90deg)\"></div><div style=\"margin-left: 20px; width: 40px; height: 20px; background: #0000ff; rotate: 90deg; transform-origin: left top\"></div>",
            120.0,
            120.0,
        );
        let blank = pixel_at(&canvas, 119, 119);
        // the 40 by 20 box turns about its center (40, 30) into a 20 by 40 box
        assert!(
            close(pixel_at(&canvas, 40, 15), [255, 0, 0]),
            "{:?}",
            pixel_at(&canvas, 40, 15)
        );
        assert_eq!(pixel_at(&canvas, 25, 30), blank);
        // the individual rotate property turns the second box about its top-left corner
        assert!(
            close(pixel_at(&canvas, 10, 75), [0, 0, 255]),
            "{:?}",
            pixel_at(&canvas, 10, 75)
        );
        assert_eq!(pixel_at(&canvas, 45, 65), blank);
    }

    #[test]
    fn clicks_land_on_scaled_and_rotated_controls() {
        let mut browser = Browser::new(bundle(
            "<style>body { margin: 0; } button { display: block; width: 40px; height: 20px; border: 0; padding: 0; }</style><button id=\"scaled\" style=\"transform: scale(2); transform-origin: 0 0\">a</button><button id=\"turned\" style=\"position: absolute; left: 100px; top: 0; transform: rotate(90deg)\">b</button><p id=\"status\">idle</p><script>for (const id of ['scaled', 'turned']) { document.getElementById(id).addEventListener('click', () => { document.getElementById('status').textContent = id; }); }</script>",
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let viewport = Size {
            width: 200.0,
            height: 120.0,
        };
        let status = |browser: &Browser| {
            let document = browser.document().expect("document");
            let status = document
                .borrow()
                .find_element_by_id("status")
                .expect("status");
            document.borrow().element_text_content(status)
        };
        assert!(browser.click_at(viewport, 70.0, 35.0).expect("click"));
        assert_eq!(status(&browser).as_deref(), Some("scaled"));
        // the rotated button stands upright around its center (120, 10)
        assert!(browser.click_at(viewport, 120.0, 25.0).expect("click"));
        assert_eq!(status(&browser).as_deref(), Some("turned"));
        // where the unrotated button would have been, nothing is hit
        let layout = browser.layout(viewport).expect("layout");
        let turned = layout.hit_test(120.0, 25.0);
        assert_ne!(layout.hit_test(137.0, 5.0), turned);
    }

    #[test]
    fn positioned_boxes_apply_their_transforms() {
        let (_, _, canvas) = image_page(
            "<style>body { margin: 0; }</style><div style=\"position: absolute; left: 10px; top: 10px; width: 20px; height: 20px; background: #0000ff; transform: translateX(30px)\"></div><div style=\"position: fixed; left: 10px; top: 60px; width: 40px; height: 10px; background: #00ff00; transform: rotate(90deg)\"></div>",
            120.0,
            120.0,
        );
        let blank = pixel_at(&canvas, 119, 119);
        assert!(close(pixel_at(&canvas, 45, 20), [0, 0, 255]));
        assert_eq!(pixel_at(&canvas, 15, 20), blank);
        assert!(
            close(pixel_at(&canvas, 30, 50), [0, 255, 0]),
            "{:?}",
            pixel_at(&canvas, 30, 50)
        );
        assert_eq!(pixel_at(&canvas, 12, 65), blank);
    }

    #[test]
    fn perspective_foreshortens_and_hidden_back_faces_disappear() {
        let mut browser = Browser::new(bundle(
            "<style>body { margin: 0; } .stage { perspective: 200px; width: 100px; height: 40px; } .card { width: 100px; height: 40px; background: #ff0000; }</style><div class=\"stage\"><div class=\"card\" style=\"transform: rotateY(60deg)\"></div></div><div class=\"stage\"><div id=\"flipped\" class=\"card\" style=\"transform: rotateY(180deg); backface-visibility: hidden\"></div></div><script>document.getElementById('flipped').addEventListener('click', () => { document.title = 'hit'; });</script>",
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let viewport = Size {
            width: 120.0,
            height: 100.0,
        };
        let canvas = browser.paint(viewport).expect("paint");
        let blank = pixel_at(&canvas, 119, 99);
        // turned 60 degrees, the card is much narrower than its 100px layout width
        assert!(close(pixel_at(&canvas, 50, 20), [255, 0, 0]));
        assert_eq!(pixel_at(&canvas, 3, 20), blank);
        assert_eq!(pixel_at(&canvas, 97, 20), blank);
        // the flipped card shows its hidden back face, so nothing paints or takes input
        assert_eq!(pixel_at(&canvas, 50, 60), blank);
        let layout = browser.layout(viewport).expect("layout");
        let hit = layout.hit_test(50.0, 60.0);
        let flipped = browser
            .document()
            .expect("document")
            .borrow()
            .find_element_by_id("flipped")
            .expect("flipped card");
        assert_ne!(hit, Some(flipped));
    }

    #[test]
    fn unsupported_or_cross_origin_images_stay_out_of_the_frame() {
        let mut resources = bundle(
            r#"<img src="/fake.png" style="display: block; width: 24px; height: 24px"><img src="https://outside.invalid/icon.svg" style="display: block; width: 24px; height: 24px">"#,
        );
        resources
            .insert("/fake.png", "image/png", vec![0, 1, 2, 3])
            .expect("fake image resource should be accepted");
        let mut browser = Browser::new(resources);
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("page should load"),
            NavigationStatus::Loaded
        );
        let layout = browser
            .layout(Size {
                width: 120.0,
                height: 80.0,
            })
            .expect("layout should build");
        assert!(layout.images.is_empty());
    }

    #[test]
    fn dynamic_pseudo_classes_follow_pointer_focus_and_checked_state() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                button { display: block; width: 120px; height: 40px; background: #111111; border: 0; }
                button:hover { background: #ff0000; }
                button:focus { border: 3px solid #00ff00; }
                input { display: block; width: 24px; height: 24px; background: #111111; }
                input:checked { background: #0000ff; }
            </style>
            <button id="go">go</button><input id="check" type="checkbox">"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("pseudo-class page should load");
        let initial = browser
            .layout(Size {
                width: 240.0,
                height: 180.0,
            })
            .expect("initial layout should build");
        assert_eq!(
            initial.body().children[0].background,
            Color::opaque(17, 17, 17)
        );
        assert_eq!(initial.body().children[0].border.widths.top, 0.0);

        assert!(
            browser
                .dispatch_at(
                    Size {
                        width: 240.0,
                        height: 180.0,
                    },
                    20.0,
                    20.0,
                    "pointermove",
                )
                .expect("pointer move should update hover")
        );
        let hovered = browser
            .layout(Size {
                width: 240.0,
                height: 180.0,
            })
            .expect("hovered layout should build");
        assert_eq!(
            hovered.body().children[0].background,
            Color::opaque(255, 0, 0)
        );

        assert!(
            browser
                .click_at(
                    Size {
                        width: 240.0,
                        height: 180.0,
                    },
                    20.0,
                    20.0,
                )
                .expect("button click should focus")
        );
        let focused = browser
            .layout(Size {
                width: 240.0,
                height: 180.0,
            })
            .expect("focused layout should build");
        assert_eq!(focused.body().children[0].border.widths.top, 3.0);

        assert!(browser.click_id("check").expect("checkbox should click"));
        let checked = browser
            .layout(Size {
                width: 240.0,
                height: 180.0,
            })
            .expect("checked layout should build");
        assert_eq!(
            checked.body().children[1].background,
            Color::opaque(0, 0, 255)
        );
    }
}
