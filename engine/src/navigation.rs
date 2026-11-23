use crate::csp::{Directive, Initiator, Policies};
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
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

mod inspector;

// the milliseconds between frames while something animates, until a window says how often
// its display refreshes
pub(crate) const FRAME_INTERVAL: u64 = 16;

// whole milliseconds between frames for a display that refreshes so many times in a thousand
// seconds, never longer than the display's own interval so no refresh goes without a frame
pub(crate) fn frame_interval(millihertz: u32) -> u64 {
    (1_000_000 / u64::from(millihertz.max(1))).clamp(4, FRAME_INTERVAL)
}

pub const APP_ORIGIN: &str = "https://app.ceri.invalid";

// files the app grants pages, served as subresources only: no document ever loads from it
pub const FILES_ORIGIN: &str = "https://files.ceri.invalid";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppUrl {
    files: bool,
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
        let files = if authority.eq_ignore_ascii_case(host_of(APP_ORIGIN)) {
            false
        } else if authority.eq_ignore_ascii_case(host_of(FILES_ORIGIN)) {
            true
        } else {
            return Err(NavigationError::WrongOrigin);
        };

        let target = &remainder[authority_end..];
        let (target, fragment) = target
            .split_once('#')
            .map_or((target, ""), |(target, fragment)| (target, fragment));
        let (raw_path, query) = target
            .split_once('?')
            .map_or((target, ""), |(path, query)| (path, query));
        let raw_path = if raw_path.is_empty() { "/" } else { raw_path };

        Ok(Self {
            files,
            path: decode_path(raw_path, files)?,
            query: query.to_string(),
            fragment: fragment.to_string(),
        })
    }

    pub fn origin(&self) -> &'static str {
        match self.files {
            true => FILES_ORIGIN,
            false => APP_ORIGIN,
        }
    }

    pub fn host(&self) -> &'static str {
        host_of(self.origin())
    }

    pub fn is_files(&self) -> bool {
        self.files
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
            files: self.files,
            path: self.path.clone(),
            query: query.into(),
            fragment: fragment.into(),
        }
    }

    pub fn with_path(&self, path: &str) -> Result<Self, NavigationError> {
        let path = if path.is_empty() { "/" } else { path };
        Self::parse(&format!("{}{path}", self.origin()))
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
        // the reference is still escaped, so the base joins it escaped too
        let base = encode_path(&self.path, self.files);
        let path = if raw_path.is_empty() {
            base.to_string()
        } else if raw_path.starts_with('/') {
            raw_path.to_string()
        } else {
            let directory = if base.ends_with('/') {
                &*base
            } else {
                base.rsplit_once('/').map_or("/", |(directory, _)| {
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
            files: self.files,
            path: resolve_reference_path(&path, self.files)?,
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
        write!(
            formatter,
            "{}{}",
            self.origin(),
            encode_path(&self.path, self.files)
        )?;
        if !self.query.is_empty() {
            write!(formatter, "?{}", self.query)?;
        }
        if !self.fragment.is_empty() {
            write!(formatter, "#{}", self.fragment)?;
        }
        Ok(())
    }
}

// the characters a decoded path may hold that would end it or start an escape in a URL
fn encode_path(path: &str, files: bool) -> std::borrow::Cow<'_, str> {
    let colon = files && path.contains(':');
    match colon || path.contains(['%', '?', '#']) {
        true => {
            let path = path
                .replace('%', "%25")
                .replace('?', "%3F")
                .replace('#', "%23");
            match colon {
                true => path.replace(':', "%3A").into(),
                false => path.into(),
            }
        }
        false => path.into(),
    }
}

fn host_of(origin: &'static str) -> &'static str {
    origin.trim_start_matches("https://")
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
// what an outside request started with spawn_external reports, in this order: the head,
// any number of chunks of the body, then done, or a failure at any point
pub enum ExternalEvent {
    Head {
        status: u16,
        mime_type: String,
        headers: BTreeMap<String, String>,
    },
    Chunk(Vec<u8>),
    Done,
    Failed(String),
}

pub type ExternalReport = Box<dyn Fn(ExternalEvent) + Send>;

#[derive(Clone)]
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
// an absolute URL whose origin is not the app's own, a mailto: or tel: link included
fn outside_app(reference: &str) -> bool {
    crate::weburl::Url::parse(reference.trim(), None)
        .is_some_and(|url| ![APP_ORIGIN, FILES_ORIGIN].contains(&url.origin().as_str()))
}

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
    if authority.eq_ignore_ascii_case(host_of(APP_ORIGIN))
        || authority.eq_ignore_ascii_case(host_of(FILES_ORIGIN))
    {
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

    // starts an outside request off the page's thread, reporting the answer as it arrives;
    // false leaves the request to provide_external, answered in place
    fn spawn_external(&self, _request: &ExternalRequest, _report: ExternalReport) -> bool {
        false
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

    // what an audio or video URL names as a decoder reads it, a file in place or bytes the app
    // already holds; none fetches it
    fn media(&self, _url: &AppUrl) -> Option<crate::MediaSource> {
        None
    }

    // how a WebSocket to a ws or wss URL reaches its server; the connector runs off the page's
    // thread, and the default refuses, so a page has no sockets the embedder did not give it
    fn connect_socket(&self, _url: &str) -> Option<SocketConnector> {
        None
    }
}

// a connected stream a WebSocket handshake runs over, and the Host header it names
pub struct SocketStream {
    pub(crate) stream: std::net::TcpStream,
    pub(crate) host: String,
}

impl SocketStream {
    pub fn new(stream: std::net::TcpStream, host: impl Into<String>) -> Self {
        Self {
            stream,
            host: host.into(),
        }
    }
}

pub type SocketConnector = Box<dyn FnOnce() -> io::Result<SocketStream> + Send>;

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

    // a picture on the clipboard as its type and encoded bytes, where the host can read one
    fn read_image(&self) -> Result<Option<(String, Vec<u8>)>, NavigationError> {
        Ok(None)
    }

    // puts an encoded picture on the clipboard in place of what was there
    fn write_image(&self, _mime_type: &str, _bytes: &[u8]) -> Result<(), NavigationError> {
        Err(NavigationError::Clipboard(
            "this clipboard takes text only".to_string(),
        ))
    }
}

// where a page's links and window.open go when they leave the app origin: the host decides what
// it opens, typically web, mail and phone links in the system's own handler; without one such a
// navigation fails, as it always did
pub trait LinkOpener {
    fn open(&self, url: &str);
}

// the modal alert, confirm and prompt a page asks for, titled with the page's title; without
// one a page goes on as headless Chrome does, with no answer from anyone
pub trait ScriptDialogs {
    fn alert(&self, title: &str, message: &str);

    fn confirm(&self, title: &str, message: &str) -> bool;

    // the text entered, none when the prompt is dismissed
    fn prompt(&self, title: &str, message: &str, default: &str) -> Option<String>;
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

    // the bundle holds the app's own files; granted files come from the provider
    pub fn resource_at(&self, url: &AppUrl) -> Option<&Resource> {
        match url.is_files() {
            true => None,
            false => self.resource(url.path()),
        }
    }

    fn load(&self, url: &AppUrl) -> Result<Resource, NavigationError> {
        if url.is_files() {
            return Err(NavigationError::WrongOrigin);
        }
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
        self.resource_at(url)
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
            Self::Colon => formatter.write_str("a files path must escape its colons"),
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
    // the parsed stylesheet, shared with the runtime so script can lay out the document to
    // measure it
    layout_inputs: Rc<RefCell<LayoutInputs>>,
    // the page clock time the last layout's animations next change what they show
    animation_wake: Cell<Option<u64>>,
    // the animated images of the document, each playing from when it was first decoded
    animated: RefCell<Vec<crate::animated::Animation>>,
    // decoded once per document and shared by every frame; failures are remembered too
}

impl Page {
    fn populate_custom_properties(&self, document: &Document, state: &mut StyleState) {
        self.layout_inputs
            .borrow()
            .stylesheet
            .clone()
            .populate_custom_properties(document, state);
    }
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
    script_dialogs: Option<Rc<dyn ScriptDialogs>>,
    link_opener: Option<Rc<dyn LinkOpener>>,
    audio_device: Option<Rc<dyn crate::AudioDevice>>,
    media_decoder: Option<Arc<dyn crate::MediaDecoder>>,
    time_zone: Option<Rc<dyn crate::TimeZone>>,
    locale: Option<String>,
    // the policy the app sets on its own documents, over whatever they declare themselves
    content_security_policy: Option<String>,
    // the display the window is on, for screen, once the window has said
    display: Option<((f32, f32), [f32; 4])>,
    // the window shows the desktop wherever the page paints nothing
    see_through: bool,
    last_navigation_failure: Option<NavigationFailure>,
    page: Option<Page>,
    current_url: Option<AppUrl>,
    history: Vec<AppUrl>,
    history_states: Vec<String>,
    history_document_ids: Vec<u64>,
    history_index: Option<usize>,
    current_document_id: Option<u64>,
    next_document_id: u64,
    inspection: inspector::Inspection,
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
    // what the last motion sample read: the document's selector version, which text edits
    // leave alone as they change no declared value, the sheet, the state without the clock
    // and the viewport, so a frame that changed none of them and has nothing moving skips
    motion_read: Option<(u64, SheetIdentity, StyleState, Size)>,
    host_runtime_info: Option<HostRuntimeInfo>,
    host_bridge: Option<crate::script::HostBridge>,
    waker: Option<crate::bridge::Waker>,
    frame_interval: u64,
    visible: bool,
    viewport: ViewportMetrics,
    preferences: UserPreferences,
    fonts: Rc<FontSet>,
    // the word or block a multi-click selected, which a following drag extends by
    selection_unit: Option<SelectionUnit>,
    // hit testing, cursor lookup and paint after one input all read the same layout
    layout_cache: RefCell<Option<CachedLayout>>,
    // the last frame and what it was painted from, so the next paints only what changed
    painted: RefCell<Option<Painted>>,
}

struct Painted {
    layout: Rc<LayoutDocument>,
    canvases: Vec<ImageLayer>,
    scale: f32,
    fonts: Rc<FontSet>,
    canvas: Arc<Canvas>,
    // the frame before `canvas`, kept while whoever was handed `canvas` still holds it, and
    // the areas that changed from it to `canvas`; the next frame is painted over it instead
    // of over a copy of `canvas`
    spare: Option<Arc<Canvas>>,
    behind: Vec<crate::layout::Rect>,
    planes: bool,
    generation: u64,
}

// a stylesheet compared by identity; holding it keeps the next sheet from reusing a replaced
// one's address while something keyed on it is kept
#[derive(Clone)]
pub(crate) struct SheetIdentity(Rc<Stylesheet>);

impl SheetIdentity {
    pub(crate) fn of(stylesheet: &Rc<Stylesheet>) -> Self {
        Self(stylesheet.clone())
    }
}

impl PartialEq for SheetIdentity {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

// everything a layout is a pure function of
#[derive(PartialEq)]
struct LayoutKey {
    document: (Option<u64>, *const RefCell<Document>, u64),
    stylesheet: SheetIdentity,
    viewport: Size,
    scale: f32,
    state: StyleState,
    selections: Vec<crate::layout::ProseSpan>,
}

struct CachedLayout {
    key: LayoutKey,
    layout: Rc<LayoutDocument>,
    // the elements in motion, when all of it only repaints them
    motion: Option<BTreeSet<NodeId>>,
    // shared by layouts that differ only in where their scrollers are scrolled to
    generation: u64,
}

fn next_generation() -> u64 {
    static GENERATIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    GENERATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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
    // the palette the system forces on pages, as windows high contrast does
    pub forced_colors: Option<crate::css::SystemColors>,
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
            script_dialogs: None,
            link_opener: None,
            audio_device: None,
            media_decoder: None,
            time_zone: None,
            locale: None,
            content_security_policy: None,
            display: None,
            see_through: false,
            last_navigation_failure: None,
            page: None,
            current_url: None,
            history: Vec::new(),
            history_states: Vec::new(),
            history_document_ids: Vec::new(),
            history_index: None,
            current_document_id: None,
            next_document_id: 1,
            inspection: Default::default(),
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
            motion_read: None,
            host_runtime_info: None,
            host_bridge: None,
            waker: None,
            frame_interval: FRAME_INTERVAL,
            visible: true,
            viewport: ViewportMetrics::default(),
            preferences: UserPreferences::default(),
            fonts: Rc::new(FontSet::builtin()),
            selection_unit: None,
            layout_cache: RefCell::new(None),
            painted: RefCell::new(None),
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

    pub fn with_link_opener(mut self, opener: Rc<dyn LinkOpener>) -> Self {
        self.link_opener = Some(opener);
        self
    }

    pub fn with_script_dialogs(mut self, dialogs: Rc<dyn ScriptDialogs>) -> Self {
        self.script_dialogs = Some(dialogs);
        self
    }

    // the speakers a page's audio contexts play through; without one they keep time silently
    pub fn with_media_decoder(mut self, decoder: Arc<dyn crate::MediaDecoder>) -> Self {
        self.media_decoder = Some(decoder);
        self
    }

    pub fn with_audio_device(mut self, device: Rc<dyn crate::AudioDevice>) -> Self {
        self.audio_device = Some(device);
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

    // inline scripts and styles written into the app's own documents run under it, as the
    // app's bundled code
    // paints frames over nothing instead of white, for a transparent window
    pub fn set_see_through(&mut self, on: bool) {
        self.see_through = on;
    }

    pub fn with_content_security_policy(mut self, policy: impl Into<String>) -> Self {
        self.content_security_policy = Some(policy.into());
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
        // a hidden page shows no transition or animation, but its workers and media go on
        let motion = ((self.visible && !self.transitions.is_empty())
            || page.runtime.has_live_workers()
            || page.runtime.media_playing())
        .then_some(self.frame_interval);
        // a stepped animation sleeps until its next step; a continuous one needs every frame
        let animation = page
            .animation_wake
            .get()
            .filter(|_| self.visible && !self.preferences.reduced_motion)
            .map(|at| match at.saturating_sub(page.runtime.now()) {
                0 => self.frame_interval,
                delay => delay,
            });
        let task = page.runtime.next_task_delay();
        let now = page.runtime.now();
        let images = page
            .animated
            .borrow()
            .iter()
            .filter(|animated| self.visible && animated.shown.get())
            .filter_map(|animated| animated.next_change(now))
            .min();
        motion
            .into_iter()
            .chain(animation)
            .chain(task)
            .chain(images)
            .min()
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

    // paces animation frames and the frames of anything in motion by the display's refresh
    pub fn set_display(&mut self, size: (f32, f32), available: [f32; 4]) {
        self.display = Some((size, available));
        if let Some(page) = self.page.as_mut() {
            page.runtime.set_display(size, available);
        }
    }

    pub fn set_refresh_rate(&mut self, millihertz: u32) {
        self.frame_interval = frame_interval(millihertz);
        if let Some(page) = self.page.as_mut() {
            page.runtime.set_frame_interval(self.frame_interval);
        }
    }

    pub(crate) fn frame_interval(&self) -> u64 {
        self.frame_interval
    }

    // whether the window shows the page; a hidden one fires visibilitychange, stops running
    // animation frames and wakes for no motion
    pub fn set_visible(&mut self, visible: bool) -> Result<(), NavigationError> {
        self.visible = visible;
        match self.page.as_mut() {
            Some(page) => page
                .runtime
                .set_visible(visible)
                .map_err(|error| NavigationError::Script(error.to_string())),
            None => Ok(()),
        }
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
            inputs.extras.forced_colors = preferences.forced_colors;
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

    // the page's stylesheet as its style elements and stylesheet links now stand
    fn stylesheet(&self, page: &Page) -> Rc<Stylesheet> {
        if let Some(base) = &self.current_url {
            refresh_stylesheet(
                &page.document.borrow(),
                &page.layout_inputs,
                base,
                &|url: &AppUrl| self.load_subresource(url),
            );
        }
        page.layout_inputs.borrow().stylesheet.clone()
    }

    fn layout_inputs(
        &self,
        stylesheet: Stylesheet,
        sheets: Vec<(SheetSource, String)>,
        document: &Document,
    ) -> Rc<RefCell<LayoutInputs>> {
        Rc::new(RefCell::new(LayoutInputs {
            stylesheet: Rc::new(stylesheet),
            sheets,
            sheets_version: document.version(),
            notes: Vec::new(),
            fonts: self.fonts.clone(),
            fonts_stamp: 0,
            extras: StyleState {
                color_scheme: self.preferences.color_scheme,
                reduced_motion: self.preferences.reduced_motion,
                forced_colors: self.preferences.forced_colors,
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
        let sent = crate::netlog::send();
        let loaded = self.find_subresource(url);
        crate::netlog::record(sent, &ResourceRequest::get(url.clone()), None, || {
            recorded(url, &loaded)
        });
        loaded
    }

    fn find_subresource(&self, url: &AppUrl) -> Result<LoadedResource, NavigationError> {
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
        if requested_url.is_files() {
            return Err(NavigationError::WrongOrigin);
        }
        let sent = crate::netlog::send();
        let loaded = self.load_navigation_request(&request);
        crate::netlog::record_navigation(sent, &request, self.next_document_id, || {
            recorded(&requested_url, &loaded)
        });
        let LoadedResource { url, resource } = match loaded {
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

        // a redirect may not land a document on the files origin either
        if url.is_files() {
            return Err(NavigationError::WrongOrigin);
        }
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
        let policies = self.document_policies(&document.borrow());
        document
            .borrow_mut()
            .set_content_security_policy(policies.clone());
        let (stylesheet_sources, script_sources) = {
            let document = document.borrow();
            let nonce = |node| document.attribute(node, "nonce").map(str::to_string);
            (
                sheet_sources(&document),
                document
                    .script_elements()
                    .into_iter()
                    .map(|(node, source)| (node, nonce(node), source))
                    .collect::<Vec<_>>(),
            )
        };
        // a stylesheet that fails to load is left out and the page renders without it
        let load = |url: &AppUrl| self.load_subresource(url);
        let mut failed_sheets = Vec::new();
        let sheets = stylesheet_sources
            .into_iter()
            .map(|source| {
                let text = load_sheet(&load, &url, &source, &policies).unwrap_or_else(|error| {
                    failed_sheets.push(format!("stylesheet failed to load: {error}"));
                    None
                });
                (source, text.unwrap_or_default())
            })
            .collect::<Vec<_>>();
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
        runtime.set_script_dialogs(self.script_dialogs.clone());
        runtime.set_audio(self.audio_device.clone(), self.waker.clone());
        runtime.set_media_decoder(self.media_decoder.clone(), self.waker.clone());
        runtime.set_socket_waker(self.waker.clone());
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
        runtime.set_frame_interval(self.frame_interval);
        runtime.start_hidden(!self.visible);
        let inspecting = crate::devtools_serving() || self.inspection.attached();
        if inspecting {
            runtime.capture_console();
            runtime.set_debugger(self.inspection.debug.clone());
        }
        for failure in failed_sheets {
            runtime.report(failure);
        }
        runtime.report_refusals();
        let (parsed_stylesheet, dropped_rules) =
            Stylesheet::parse_recovering(&joined_sheets(&sheets));
        for dropped in dropped_rules {
            runtime.report(format!("dropped CSS rule: {dropped}"));
        }
        load_web_fonts(&load, &self.fonts, &url, &parsed_stylesheet, &policies);
        runtime.report_refusals();
        let layout_inputs = self.layout_inputs(parsed_stylesheet, sheets, &document.borrow());
        runtime.set_layout_inputs(layout_inputs.clone());
        runtime.load_document_images();
        if let HistoryCommit::Traverse { state, .. } = &commit {
            runtime
                .set_history_state_serialized(state)
                .map_err(|error| NavigationError::Script(error.to_string()))?;
        }
        let html = inspecting.then_some(source.as_str());
        if inspecting {
            self.announce_document(&url, &mut runtime);
        }
        self.execute_scripts(&url, html, &script_sources, &policies, &mut runtime);
        if let Err(error) = runtime.apply_autofocus() {
            let error = NavigationError::Script(error.to_string());
            let status = recovery_status(&error);
            self.install_error_page(&url, status, &error.to_string());
            self.commit_new_document(url, commit);
            return Ok(NavigationStatus::ErrorPage { status });
        }
        self.persist_storage()?;

        self.page = Some(Page {
            document,
            runtime,
            layout_inputs,
            animation_wake: Cell::new(None),
            animated: RefCell::default(),
        });
        self.last_navigation_failure = None;
        self.hovered_node = None;
        self.hovered_thumb = None;
        self.commit_new_document(url, commit);
        Ok(self
            .follow_pending_navigations(redirect_depth)?
            .unwrap_or(NavigationStatus::Loaded))
    }

    pub fn current_url(&self) -> Option<&AppUrl> {
        self.current_url.as_ref()
    }

    pub fn last_navigation_failure(&self) -> Option<&NavigationFailure> {
        self.last_navigation_failure.as_ref()
    }

    // a page with a running animation or transition needs a frame per clock tick
    pub fn has_active_animations(&self) -> bool {
        // animated images play whatever the motion preference, as in browsers
        let images = self.page.as_ref().is_some_and(|page| {
            let now = page.runtime.now();
            page.animated
                .borrow()
                .iter()
                .any(|animated| animated.shown.get() && animated.next_change(now).is_some())
        });
        if self.preferences.reduced_motion {
            return images;
        }
        let animating = self
            .page
            .as_ref()
            .is_some_and(|page| page.animation_wake.get().is_some());
        images || animating || !self.transitions.is_empty()
    }

    // records which animatable values changed, so layout can blend from the old ones
    pub fn sample_transitions(&mut self, viewport: Size) -> Result<bool, NavigationError> {
        let _profile = crate::profile::scope("frame: motion");
        let Some(page) = self.page.as_ref() else {
            return Ok(false);
        };
        let stylesheet = self.stylesheet(page);
        let has_motion = stylesheet.has_transitions() || stylesheet.has_animations();
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
        state.forced_colors = self.preferences.forced_colors;
        let read = (
            document.selector_version(),
            SheetIdentity::of(&stylesheet),
            state.clone(),
            viewport,
        );
        state.clock = clock;
        // the declared values can only differ when something they read did, and a frame with
        // nothing in motion has no event to fire either
        let settled = self.transitions.is_empty() && self.animations.values().all(|run| run.ended);
        if settled && self.motion_read.as_ref() == Some(&read) {
            return Ok(false);
        }
        self.motion_read = Some(read);

        let mut seen = BTreeSet::new();
        let mut seen_animations = BTreeSet::new();
        let mut events = Vec::new();
        let mut changed = false;
        let candidates = collect_motion_candidates(&stylesheet, &state, &document);
        // custom properties cost a document-wide pass, so only pages with moving elements pay it
        if !candidates.is_empty() {
            page.populate_custom_properties(&document, &mut state);
        }
        for node in candidates {
            let values = stylesheet.declared_values(StyleContext {
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
                .filter(|animation| stylesheet.has_keyframes(&animation.name))
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

    // whether the page has an element shown fullscreen, which the window follows
    pub fn fullscreen(&self) -> bool {
        self.page
            .as_ref()
            .is_some_and(|page| page.runtime.fullscreen_requested())
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
        let hit = self.layout(viewport)?.hit_test(x, y);
        let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
        let Some(node) = page.runtime.click_target(hit) else {
            return Ok(false);
        };
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
        let _fonts = crate::font::use_fonts(self.fonts.clone(), self.viewport.device_pixel_ratio);
        self.layout(viewport).ok()?.thumb_drag_at(x, y)
    }

    // whether a press at a point lands in an app-region drag area, which moves the window
    // instead of reaching the page
    pub fn drag_region_at(&self, viewport: Size, x: f32, y: f32) -> bool {
        let Some(page) = self.page.as_ref() else {
            return false;
        };
        let _fonts = crate::font::use_fonts(self.fonts.clone(), self.viewport.device_pixel_ratio);
        let Some(node) = self
            .layout(viewport)
            .ok()
            .and_then(|layout| layout.hit_test(x, y))
        else {
            return false;
        };
        let document = page.document.borrow();
        let Some(element) = std::iter::once(node)
            .chain(document.ancestors(node))
            .find(|&node| document.element(node).is_some())
        else {
            return false;
        };
        let mut state = page.runtime.style_state(self.hovered_node);
        state.viewport_width = viewport.width;
        state.viewport_height = viewport.height;
        state.color_scheme = self.preferences.color_scheme;
        state.reduced_motion = self.preferences.reduced_motion;
        state.forced_colors = self.preferences.forced_colors;
        self.stylesheet(page)
            .computed_style(&document, element, &state)
            .is_some_and(|(style, _)| style.drag_region)
    }

    // the element a press here would drag: the nearest one whose draggable is true, which by
    // default an image or a link is
    pub fn drag_source_at(&self, viewport: Size, x: f32, y: f32) -> Option<NodeId> {
        let page = self.page.as_ref()?;
        let _fonts = crate::font::use_fonts(self.fonts.clone(), self.viewport.device_pixel_ratio);
        let node = self.layout(viewport).ok()?.hit_test(x, y)?;
        let document = page.document.borrow();
        std::iter::once(node)
            .chain(document.ancestors(node))
            .find(|&node| {
                let Some(element) = document.element(node) else {
                    return false;
                };
                match element.attr("draggable").map(str::to_ascii_lowercase) {
                    Some(value) if value == "true" => true,
                    Some(value) if value == "false" => false,
                    _ => {
                        element.namespace == crate::dom::Namespace::Html
                            && (element.name == "img"
                                || element.name == "a" && element.attr("href").is_some())
                    }
                }
            })
    }

    // dragstart reports where the press began, as Blink fires it with the press event
    pub fn start_drag(&mut self, source: NodeId, x: f32, y: f32) -> Result<bool, NavigationError> {
        let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
        page.runtime.set_pointer_position(x, y);
        let handled = page
            .runtime
            .dispatch_drag_start(source)
            .map_err(|error| NavigationError::Script(error.to_string()))?;
        Ok(self.process_pending_navigation()?.is_some() || handled)
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
            let _fonts =
                crate::font::use_fonts(self.fonts.clone(), self.viewport.device_pixel_ratio);
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
            let _fonts =
                crate::font::use_fonts(self.fonts.clone(), self.viewport.device_pixel_ratio);
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
            let _fonts =
                crate::font::use_fonts(self.fonts.clone(), self.viewport.device_pixel_ratio);
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
            let _fonts =
                crate::font::use_fonts(self.fonts.clone(), self.viewport.device_pixel_ratio);
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
            let _fonts =
                crate::font::use_fonts(self.fonts.clone(), self.viewport.device_pixel_ratio);
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
        let node = page.runtime.pointer_target(node).map_err(script_error)?;
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
        // a press anywhere, even on no element, may close the auto popovers it is outside of
        if event == "pointerdown" {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            let target = node.or_else(|| page.runtime.document().borrow().document_element());
            if let Some(target) = target {
                page.runtime
                    .light_dismiss_popovers(target)
                    .map_err(|error| NavigationError::Script(error.to_string()))?;
            }
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
            let executed = page
                .runtime
                .advance_time(milliseconds)
                .map_err(|error| NavigationError::Script(error.to_string()))?;
            page.runtime.report_refusals();
            executed
        };
        self.process_pending_navigation()?;
        Ok(executed)
    }

    pub fn pump(&mut self) -> Result<usize, NavigationError> {
        let processed = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            let processed = page
                .runtime
                .pump()
                .map_err(|error| NavigationError::Script(error.to_string()))?;
            page.runtime.report_refusals();
            processed
        };
        self.process_pending_navigation()?;
        Ok(processed)
    }

    pub fn console_logs(&mut self) -> &[String] {
        match &mut self.page {
            Some(page) => {
                page.runtime.report_refusals();
                page.runtime.logs()
            }
            None => &[],
        }
    }

    // the app's policy over its own documents, then any the document declares itself
    fn document_policies(&self, document: &Document) -> Rc<Policies> {
        let mut policies = Policies::default();
        if let Some(policy) = &self.content_security_policy {
            let scripts = document.script_elements();
            let stylesheets = document.stylesheet_elements();
            let inline = scripts
                .iter()
                .filter_map(|(_, source)| match source {
                    ScriptSource::Inline(text)
                    | ScriptSource::InlineModule(text)
                    | ScriptSource::ImportMap(text) => Some(text.as_str()),
                    _ => None,
                })
                .chain(stylesheets.iter().filter_map(|(_, source)| match source {
                    StylesheetSource::Inline(text) => Some(text.as_str()),
                    StylesheetSource::External(_) | StylesheetSource::Cssom(_) => None,
                }));
            policies.add_bundled(policy, inline);
        }
        for policy in document.meta_policies() {
            policies.add(&policy);
        }
        Rc::new(policies)
    }

    pub fn layout(&self, viewport: Size) -> Result<Rc<LayoutDocument>, NavigationError> {
        let _profile = crate::profile::scope("frame: layout");
        let page = self.page.as_ref().ok_or(NavigationError::NoPage)?;
        let stylesheet = &self.stylesheet(page);
        let document = page.document.clone();
        let document = document.borrow();
        let mut style_state = page.runtime.style_state(self.hovered_node);
        style_state.hovered_part = self
            .hovered_thumb
            .map(|node| (node, crate::css::PseudoElement::ScrollbarThumb));
        style_state.viewport_width = viewport.width;
        style_state.viewport_height = viewport.height;
        style_state.color_scheme = self.preferences.color_scheme;
        style_state.reduced_motion = self.preferences.reduced_motion;
        style_state.forced_colors = self.preferences.forced_colors;
        style_state.clock = page.runtime.now();
        style_state.transitions = self.transitions.clone();
        style_state.animation_starts = self
            .animations
            .iter()
            .map(|(node, run)| (*node, run.started))
            .collect();
        let _fonts = crate::font::use_fonts(self.fonts.clone(), self.viewport.device_pixel_ratio);
        // found once per change to the document's elements, not walked for on every frame
        let lists = page.runtime.element_lists();
        if let Some(base) = self.current_url.as_ref() {
            for &node in &lists[4] {
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
        for &node in &lists[2] {
            let size = crate::script::canvas_size(&document, node);
            style_state.image_sizes.insert(node, size);
        }
        for &node in &lists[3] {
            style_state
                .image_sizes
                .insert(node, page.runtime.video_size(node));
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
            stylesheet: SheetIdentity::of(stylesheet),
            viewport,
            scale: self.viewport.device_pixel_ratio,
            state: keyed_state,
            selections: page.runtime.prose_selections(),
        };
        if let Some(cached) = self.layout_cache.borrow().as_ref()
            && cached.key == key
        {
            return Ok(cached.layout.clone());
        }
        let scrolled = self.layout_cache.borrow().as_ref().and_then(|cached| {
            let last = &cached.key;
            let scroll_only = last.document == key.document
                && last.stylesheet == key.stylesheet
                && last.viewport == key.viewport
                && last.scale == key.scale
                && last.selections == key.selections
                && last.state
                    == StyleState {
                        scroll_offsets: last.state.scroll_offsets.clone(),
                        ..key.state.clone()
                    };
            scroll_only
                .then(|| crate::layout::rescroll(&cached.layout, &key.state.scroll_offsets))?
                .map(|layout| (Rc::new(layout), cached.motion.clone(), cached.generation))
        });
        if let Some((layout, motion, generation)) = scrolled {
            *self.layout_cache.borrow_mut() = Some(CachedLayout {
                key,
                layout: layout.clone(),
                motion,
                generation,
            });
            return Ok(layout);
        }
        page.populate_custom_properties(&document, &mut style_state);
        let repainted = self.layout_cache.borrow().as_ref().and_then(|cached| {
            let last = &cached.key;
            let clock = last.state.clock;
            let clock_only = last.document == key.document
                && last.stylesheet == key.stylesheet
                && last.viewport == key.viewport
                && last.scale == key.scale
                && last.selections == key.selections
                && last.state
                    == StyleState {
                        clock,
                        ..key.state.clone()
                    };
            let nodes = cached.motion.as_ref().filter(|_| clock_only)?;
            let last_state = StyleState {
                clock,
                ..style_state.clone()
            };
            crate::layout::repaint_motion(
                &document,
                stylesheet,
                viewport,
                [&last_state, &style_state],
                (nodes, &cached.layout),
            )
            .map(|layout| (layout, cached.motion.clone()))
        });
        if let Some((layout, motion)) = repainted {
            page.animation_wake.set(
                crate::css::take_animation_wake()
                    .map(|delay| style_state.clock.saturating_add(delay)),
            );
            let layout = Rc::new(layout);
            *self.layout_cache.borrow_mut() = Some(CachedLayout {
                key,
                layout: layout.clone(),
                motion,
                generation: next_generation(),
            });
            return Ok(layout);
        }
        crate::css::take_animation_wake();
        crate::css::record_motion();
        let mut layout = build_layout(&document, stylesheet, viewport, &style_state);
        // container queries read sizes from a layout, so a change in them lays out once more
        if stylesheet.has_container_queries() {
            let measured = crate::layout::take_layout_containers();
            if measured != style_state.container_sizes {
                style_state.container_sizes = measured;
                page.populate_custom_properties(&document, &mut style_state);
                layout = build_layout(&document, stylesheet, viewport, &style_state);
            }
            *self.container_sizes.borrow_mut() = style_state.container_sizes.clone();
        }
        let motion = crate::css::take_motion();
        page.animation_wake.set(
            crate::css::take_animation_wake().map(|delay| style_state.clock.saturating_add(delay)),
        );
        page.layout_inputs.borrow_mut().extras = StyleState {
            hovered: self.hovered_node,
            color_scheme: style_state.color_scheme,
            reduced_motion: style_state.reduced_motion,
            forced_colors: style_state.forced_colors,
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
        *self.layout_cache.borrow_mut() = Some(CachedLayout {
            key,
            layout: layout.clone(),
            motion,
            generation: next_generation(),
        });
        Ok(layout)
    }

    pub fn paint(&self, viewport: Size) -> Result<Arc<Canvas>, NavigationError> {
        self.paint_scaled(viewport, 1.0)
    }

    // drops what is held only to make the next frame cheaper while frames follow each
    // other closely: the second canvas and the layout answers kept from the last layout
    pub(crate) fn rest(&self) {
        if let Some(painted) = self.painted.borrow_mut().as_mut() {
            painted.spare = None;
            painted.behind.clear();
        }
        crate::layout::forget_kept_answers();
    }

    pub fn paint_scaled(
        &self,
        viewport: Size,
        device_pixel_ratio: f32,
    ) -> Result<Arc<Canvas>, NavigationError> {
        self.paint_planes(viewport, device_pixel_ratio, false)
            .map(|(canvas, _)| canvas)
    }

    // with planes, a video a presenter can show under the canvas comes back as its frame and
    // where it goes, and the canvas leaves its box transparent
    pub(crate) fn paint_planes(
        &self,
        viewport: Size,
        device_pixel_ratio: f32,
        planes: bool,
    ) -> Result<(Arc<Canvas>, Vec<crate::paint::PlaneFrame>), NavigationError> {
        let _profile = crate::profile::scope("frame: paint");
        let layout = self.layout(viewport)?;
        let mut canvases = self
            .page
            .as_ref()
            .map(|page| {
                let mut layers = page.runtime.canvas_layers();
                layers.extend(page.runtime.video_layers());
                layers.extend(animated_layers(page, &layout));
                layers
            })
            .unwrap_or_default();
        let _fonts = crate::font::use_fonts(self.fonts.clone(), device_pixel_ratio);
        let generation = self
            .layout_cache
            .borrow()
            .as_ref()
            .map_or(0, |cached| cached.generation);
        let mut painted = self.painted.borrow_mut();
        let reusable = |last: &Painted| {
            last.scale == device_pixel_ratio
                && last.planes == planes
                && Rc::ptr_eq(&last.fonts, &self.fonts)
        };
        let scrolled = painted
            .as_ref()
            .filter(|last| {
                reusable(last)
                    && last.generation == generation
                    && last.canvases.len() == canvases.len()
                    && last.canvases.iter().zip(&canvases).all(|(before, after)| {
                        before.node == after.node
                            && before.background == after.background
                            && Arc::ptr_eq(&before.image, &after.image)
                    })
            })
            // moved pixels are filled in behind over a flat colour, which a clear frame lacks
            .filter(|_| !self.see_through)
            .and_then(|last| crate::paint::scroll_repaint(&last.canvas, &last.layout, &layout));
        let damage = match painted.as_ref() {
            _ if scrolled.is_some() => crate::paint::Damage::Nothing,
            Some(last) if reusable(last) => crate::paint::damage(
                (&last.layout, &last.canvases),
                (&layout, &canvases),
                last.canvas.planes(),
            ),
            _ => crate::paint::Damage::Everything,
        };
        let base = if self.see_through {
            crate::css::Color::transparent()
        } else {
            crate::paint::BASE
        };
        let (mut canvas, mut spare, behind) = match (damage, painted.take()) {
            (crate::paint::Damage::Nothing, Some(mut last)) => match scrolled {
                Some(plan) => {
                    crate::paint::scroll(Arc::make_mut(&mut last.canvas), plan, &layout, &canvases);
                    (last.canvas, None, Vec::new())
                }
                None => (last.canvas, last.spare, last.behind),
            },
            (crate::paint::Damage::Areas(areas), Some(mut last)) => {
                let shown = last.canvas.revision().id;
                let spare = last
                    .spare
                    .take()
                    .filter(|spare| Arc::strong_count(spare) == 1);
                if let Some(canvas) = Arc::get_mut(&mut last.canvas) {
                    crate::paint::repaint_areas(canvas, &layout, &canvases, &areas);
                    (last.canvas, None, Vec::new())
                } else if let Some(mut spare) = spare {
                    // the frame before the last is let go of by now, so it catches up and
                    // the two swap
                    crate::paint::repaint_behind(
                        Arc::make_mut(&mut spare),
                        &layout,
                        &canvases,
                        (&last.behind, &areas),
                        shown,
                    );
                    (spare, Some(last.canvas), areas)
                } else {
                    let held = last.canvas.clone();
                    let canvas = Arc::make_mut(&mut last.canvas);
                    crate::paint::repaint_areas(canvas, &layout, &canvases, &areas);
                    (last.canvas, Some(held), areas)
                }
            }
            _ => (
                Arc::new(crate::paint::paint_frame(
                    &layout,
                    &canvases,
                    device_pixel_ratio,
                    planes,
                    base,
                )),
                None,
                Vec::new(),
            ),
        };
        let unread = |layer: &ImageLayer, canvas: &Canvas| {
            layer.image.pixels.len() < layer.image.width as usize * layer.image.height as usize
                && !canvas.planes().iter().any(|plane| plane.node == layer.node)
        };
        if let Some(page) = &self.page
            && canvases.iter().any(|layer| unread(layer, &canvas))
        {
            for layer in &mut canvases {
                if unread(layer, &canvas)
                    && let Some(image) = page.runtime.video_content(layer.node)
                {
                    layer.image = image;
                }
            }
            canvas = Arc::new(crate::paint::paint_frame(
                &layout,
                &canvases,
                device_pixel_ratio,
                planes,
                base,
            ));
            spare = None;
        }
        let shown = canvas
            .planes()
            .iter()
            .filter_map(|plane| {
                let layer = canvases
                    .iter()
                    .find(|layer| layer.node == plane.node && layer.background.is_none())?;
                let surface = self.page.as_ref()?.runtime.video_surface(plane.node);
                Some((plane.clone(), layer.image.clone(), surface))
            })
            .collect();
        if let Some(page) = &self.page {
            page.runtime.mark_planes(canvas.planes());
        }
        *painted = Some(Painted {
            layout,
            canvases,
            scale: device_pixel_ratio,
            fonts: self.fonts.clone(),
            canvas: canvas.clone(),
            spare,
            behind,
            planes,
            generation,
        });
        // the highlight goes on a copy, so the kept frame repaints and scrolls without it
        let canvas = match self.inspection.fills() {
            Some(fills) => Arc::new(canvas.overlaid(fills)),
            None => canvas,
        };
        Ok((canvas, shown))
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
    fn image_for(&self, page: &Page, base: &AppUrl, reference: &str) -> Option<Arc<Image>> {
        let reference = reference.trim();
        let key = crate::script::image_key(base, reference)?;
        if let Some(cached) = page.runtime.image_cache().borrow().get(&key) {
            return cached.clone();
        }
        let policies = page.document.borrow().content_security_policy().clone();
        let permitted =
            |url: &str| policies.permits(policies.url(Directive::Img, url, Initiator::default()));
        let held = crate::image::decode_data_url(&key).or_else(|| page.runtime.object_url(&key));
        let encoded = if let Some((mime_type, bytes)) = held {
            permitted(&key).then_some((mime_type, bytes))
        } else {
            base.resolve(reference)
                .ok()
                .filter(|url| permitted(&url.to_string()))
                .and_then(|url| self.load_subresource(&url).ok())
                .map(|resource| (resource.mime_type.clone(), resource.body().to_vec()))
        };
        let decoded = encoded
            .as_ref()
            .and_then(|(mime_type, bytes)| crate::image::decode_image(bytes, mime_type).ok())
            .map(Arc::new);
        if let (Some((_, bytes)), Some(image)) = (&encoded, &decoded)
            && let Some(animation) =
                crate::animated::Animation::new(bytes, image.clone(), page.runtime.now())
        {
            page.animated.borrow_mut().push(animation);
        }
        page.runtime
            .image_cache()
            .borrow_mut()
            .insert(key, decoded.clone());
        decoded
    }

    fn activate_node(&mut self, node: NodeId) -> Result<bool, NavigationError> {
        let handled = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            // a click the host makes for its user activates the page as a press would
            page.runtime.note_activation();
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
        self.follow_pending_navigations(0)
    }

    fn follow_pending_navigations(
        &mut self,
        redirect_depth: usize,
    ) -> Result<Option<NavigationStatus>, NavigationError> {
        let mut status = None;
        while let Some(request) = self
            .page
            .as_mut()
            .ok_or(NavigationError::NoPage)?
            .runtime
            .take_navigation_request()
        {
            status = Some(self.follow_navigation(request, redirect_depth)?);
        }
        Ok(status)
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
                // a javascript: link stays on the page, as void(0) and friends ask
                if reference
                    .trim_start()
                    .get(..11)
                    .is_some_and(|scheme| scheme.eq_ignore_ascii_case("javascript:"))
                {
                    return Ok(NavigationStatus::Loaded);
                }
                // the page stays where it is and a link out of the app opens outside it
                if outside_app(&reference)
                    && let Some(opener) = &self.link_opener
                {
                    opener.open(reference.trim());
                    return Ok(NavigationStatus::Loaded);
                }
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
            // window.open: a page outside the app opens in the host's handler; the app has one
            // window per page, so an app page opens nowhere new
            NavigationRequest::Open { reference } => {
                if outside_app(&reference)
                    && let Some(opener) = &self.link_opener
                {
                    opener.open(reference.trim());
                }
                Ok(NavigationStatus::Loaded)
            }
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
        // history.go(0) and location.reload() load the current entry again
        if delta != 0 && self.current_document_id == Some(document_id) {
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

    // a script that fails to load or run is reported and the rest of the page goes on, as in
    // browsers
    // html is the document's source while a debugger may want to know where in it each
    // script was written
    fn execute_scripts(
        &self,
        base: &AppUrl,
        html: Option<&str>,
        sources: &[(NodeId, Option<String>, ScriptSource)],
        policies: &Policies,
        runtime: &mut ScriptRuntime,
    ) {
        let mut module_loader = PackagedModuleLoader {
            resources: &self.resources,
            provider: self.resource_provider.as_deref(),
            policies,
        };
        let mut cursor = 0;
        for (node, nonce, source) in sources {
            let element = Initiator {
                nonce: nonce.as_deref(),
                parser_inserted: true,
            };
            if let Some(html) = html {
                let origin = match source {
                    ScriptSource::Inline(text) | ScriptSource::InlineModule(text) => html
                        .get(cursor..)
                        .and_then(|rest| rest.find(text.as_str()))
                        .map(|found| {
                            cursor += found + text.len();
                            let (line, column) = place_in(html, cursor - text.len());
                            (base.to_string(), line, column)
                        }),
                    ScriptSource::External(reference) => base
                        .resolve(reference)
                        .ok()
                        .map(|url| (url.to_string(), 0, 0)),
                    _ => None,
                };
                if let Some((url, line, column)) = origin {
                    runtime.set_script_origin(url, line, column);
                }
            }
            let classic = matches!(source, ScriptSource::Inline(_) | ScriptSource::External(_));
            runtime.set_current_script(classic.then_some(*node));
            if let Err(error) =
                self.execute_script(base, source, element, &mut module_loader, runtime)
            {
                runtime.report(format!("script failed to load: {error}"));
            }
            runtime.set_current_script(None);
            runtime.clear_script_origin();
        }
    }

    fn execute_script(
        &self,
        base: &AppUrl,
        source: &ScriptSource,
        element: Initiator,
        module_loader: &mut PackagedModuleLoader,
        runtime: &mut ScriptRuntime,
    ) -> Result<(), NavigationError> {
        let policies = module_loader.policies;
        let check = match source {
            ScriptSource::Inline(text)
            | ScriptSource::InlineModule(text)
            | ScriptSource::ImportMap(text) => {
                policies.inline(Directive::ScriptElem, text, element)
            }
            ScriptSource::External(reference) | ScriptSource::ExternalModule(reference) => {
                let url = base.resolve(reference)?;
                policies.url(Directive::ScriptElem, &url.to_string(), element)
            }
        };
        if !runtime.permits(check) {
            return Ok(());
        }
        let ran = match source {
            ScriptSource::Inline(source) => runtime.execute_reporting(source),
            ScriptSource::External(reference) => {
                let url = base.resolve(reference)?;
                let resource = self.load_subresource(&url)?;
                if !is_javascript(&resource) {
                    return Err(NavigationError::UnsupportedMimeType {
                        path: url.path().to_string(),
                        mime_type: resource.mime_type,
                    });
                }
                let source = String::from_utf8(resource.body)
                    .map_err(|_| NavigationError::InvalidUtf8(url.path().to_string()))?;
                runtime.execute_reporting(&source)
            }
            ScriptSource::ImportMap(source) => {
                runtime.add_import_map(source, base);
                Ok(())
            }
            ScriptSource::InlineModule(source) => {
                runtime.execute_module_reporting(source, base.clone(), module_loader)
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
                runtime.execute_module_reporting(&source, loaded_url, module_loader)
            }
        };
        ran.map_err(|error| NavigationError::Script(error.to_string()))
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
        let sheets = sheet_sources(&document.borrow())
            .into_iter()
            .map(|source| {
                let text = match &source.1 {
                    StylesheetSource::Inline(text) | StylesheetSource::Cssom(text) => text.clone(),
                    StylesheetSource::External(_) => String::new(),
                };
                (source, text)
            })
            .collect::<Vec<_>>();
        let stylesheet = Stylesheet::parse_recovering(&joined_sheets(&sheets)).0;
        let layout_inputs = self.layout_inputs(stylesheet, sheets, &document.borrow());
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
                runtime.set_script_dialogs(self.script_dialogs.clone());
                runtime.set_audio(self.audio_device.clone(), self.waker.clone());
                runtime.set_media_decoder(self.media_decoder.clone(), self.waker.clone());
                runtime.set_socket_waker(self.waker.clone());
                if let Some(time_zone) = &self.time_zone {
                    runtime.set_time_zone(time_zone.clone());
                }
                if let Some(locale) = &self.locale {
                    runtime.set_locale(locale.clone());
                }
                runtime.set_viewport_metrics(self.viewport);
                runtime.set_frame_interval(self.frame_interval);
                runtime.set_layout_inputs(layout_inputs.clone());
                runtime
            },
            document,
            layout_inputs,
            animation_wake: Cell::new(None),
            animated: RefCell::default(),
        });
    }
}

struct PackagedModuleLoader<'a> {
    resources: &'a ResourceBundle,
    provider: Option<&'a dyn ResourceProvider>,
    policies: &'a Policies,
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
        crate::script::permit_module(self.policies, &url)?;
        let sent = crate::netlog::send();
        let loaded = match self.resources.load_subresource(&url) {
            Ok(resource) => Ok(LoadedResource {
                url: url.clone(),
                resource,
            }),
            Err(NavigationError::ResourceNotFound(path)) => self
                .provider
                .map(|provider| load_provider_resource(provider, &url))
                .transpose()
                .and_then(|loaded| {
                    let (url, resource) = loaded
                        .flatten()
                        .ok_or(NavigationError::ResourceNotFound(path))?;
                    Ok(LoadedResource { url, resource })
                }),
            Err(error) => Err(error),
        };
        crate::netlog::record(
            sent,
            &ResourceRequest::get(url.clone()),
            Some("Script"),
            || recorded(&url, &loaded),
        );
        let LoadedResource { url, resource } = loaded.map_err(|error| match error {
            NavigationError::ResourceNotFound(path) => {
                runtime_error(format!("module {path} was not found"))
            }
            error => runtime_error(error.to_string()),
        })?;
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

// what a load gave, as the network log keeps it
fn recorded(
    url: &AppUrl,
    loaded: &Result<LoadedResource, NavigationError>,
) -> Result<crate::netlog::Reply, String> {
    match loaded {
        Ok(loaded) => Ok(crate::netlog::Reply::resource(
            &loaded.url,
            &loaded.resource,
        )),
        Err(NavigationError::ResourceNotFound(_)) => Ok(crate::netlog::Reply::status(url, 404)),
        Err(NavigationError::HttpStatus { status, .. }) => {
            Ok(crate::netlog::Reply::status(url, *status))
        }
        Err(error) => Err(error.to_string()),
    }
}

fn is_html(resource: &Resource) -> bool {
    resource
        .mime_type
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html"))
}

pub(crate) type LoadResource<'a> = &'a dyn Fn(&AppUrl) -> Result<Resource, NavigationError>;
pub(crate) type SheetSource = (Option<String>, StylesheetSource);

// the stylesheet elements a document has, with their nonces, in tree order
pub(crate) fn sheet_sources(document: &Document) -> Vec<SheetSource> {
    document
        .stylesheet_elements()
        .into_iter()
        .map(|(node, source)| {
            (
                document.attribute(node, "nonce").map(str::to_string),
                source,
            )
        })
        .collect()
}

// one style element's or stylesheet link's text with its imports spliced in, or none when the
// policy refuses it
fn load_sheet(
    load: LoadResource,
    base: &AppUrl,
    (nonce, source): &SheetSource,
    policies: &Policies,
) -> Result<Option<String>, NavigationError> {
    let element = Initiator {
        nonce: nonce.as_deref(),
        parser_inserted: true,
    };
    match source {
        StylesheetSource::Cssom(text) => Ok(Some(text.clone())),
        StylesheetSource::Inline(source) => {
            if !policies.permits(policies.inline(Directive::StyleElem, source, element)) {
                return Ok(None);
            }
            Ok(Some(expand_imports(
                load,
                source.clone(),
                base,
                0,
                policies,
            )))
        }
        StylesheetSource::External(reference) => {
            let url = base.resolve(reference)?;
            if !policies.permits(policies.url(Directive::StyleElem, &url.to_string(), element)) {
                return Ok(None);
            }
            let resource = load(&url)?;
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
            Ok(Some(expand_imports(load, text, &url, 0, policies)))
        }
    }
}

// a byte offset in a document as the devtools protocol's zero-based line and UTF-16 column
fn place_in(html: &str, offset: usize) -> (u32, u32) {
    let before = &html[..offset];
    let line = before.matches('\n').count() + before.matches('\r').count()
        - before.matches("\r\n").count();
    let start = before.rfind(['\n', '\r']).map_or(0, |found| found + 1);
    let column = before[start..].encode_utf16().count();
    (line as u32, column as u32)
}

fn joined_sheets(sheets: &[(SheetSource, String)]) -> String {
    sheets
        .iter()
        .map(|(_, text)| text.as_str())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

// splices each @import's stylesheet in place of the rule, inside the layer, supports and
// media conditions it names; one that fails to load is skipped, as a browser does
fn expand_imports(
    load: LoadResource,
    text: String,
    base: &AppUrl,
    depth: usize,
    policies: &Policies,
) -> String {
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
        if !policies.permits(policies.url(
            Directive::StyleElem,
            &url.to_string(),
            Initiator::default(),
        )) {
            continue;
        }
        let Ok(resource) = load(&url) else {
            continue;
        };
        if !is_css(&resource) {
            continue;
        }
        let Ok(body) = String::from_utf8(resource.body) else {
            continue;
        };
        expanded.push_str(&import.open);
        expanded.push_str(&expand_imports(load, body, &url, depth + 1, policies));
        expanded.push_str(&import.close);
        expanded.push('\n');
    }
    expanded.push_str(&rest);
    expanded
}

// fetches each @font-face rule's first source that loads and parses; a rule with none is
// skipped, and its family falls back to installed faces
fn load_web_fonts(
    load: LoadResource,
    fonts: &FontSet,
    base: &AppUrl,
    stylesheet: &Stylesheet,
    policies: &Policies,
) {
    fonts.clear_web_faces();
    for rule in stylesheet.font_faces() {
        let face = rule.sources.iter().find_map(|source| {
            let url = base.resolve(source).ok()?;
            let check = policies.url(Directive::Font, &url.to_string(), Initiator::default());
            if !policies.permits(check) {
                return None;
            }
            let resource = load(&url).ok()?;
            crate::font::FontFace::parse(resource.body).ok()
        });
        if let Some(face) = face {
            fonts.add_web_face(
                &rule.family,
                rule.weights,
                rule.italic,
                rule.ranges.clone(),
                Rc::new(face),
            );
        }
    }
}

// the faces script loaded through FontFace and added to document.fonts, beside the sheet's own
fn add_script_fonts(document: &Document, inputs: &mut LayoutInputs) {
    let (fonts, stamp) = document.script_fonts();
    for font in fonts {
        inputs.fonts.add_web_face(
            &font.family,
            font.weights,
            font.italic,
            Vec::new(),
            font.face.clone(),
        );
    }
    inputs.fonts_stamp = stamp;
}

// rebuilds the sheet once script added, removed or rewrote a style element or stylesheet link;
// a sheet still in place keeps the text it loaded with, and one that fails to load stays empty
pub(crate) fn refresh_stylesheet(
    document: &Document,
    inputs: &RefCell<LayoutInputs>,
    base: &AppUrl,
    load: LoadResource,
) {
    if inputs.borrow().sheets_version == document.version() {
        return;
    }
    let sources = sheet_sources(document);
    let mut inputs = inputs.borrow_mut();
    inputs.sheets_version = document.version();
    let fonts_changed = document.script_fonts().1 != inputs.fonts_stamp;
    if inputs.sheets.len() == sources.len()
        && inputs
            .sheets
            .iter()
            .zip(&sources)
            .all(|((old, _), source)| old == source)
    {
        if fonts_changed {
            let policies = document.content_security_policy().clone();
            let stylesheet = inputs.stylesheet.clone();
            load_web_fonts(load, &inputs.fonts, base, &stylesheet, &policies);
            add_script_fonts(document, &mut inputs);
        }
        return;
    }
    let policies = document.content_security_policy().clone();
    let mut sheets = Vec::with_capacity(sources.len());
    for source in sources {
        let text = match inputs.sheets.iter().find(|(old, _)| *old == source) {
            Some((_, text)) => text.clone(),
            None => load_sheet(load, base, &source, &policies)
                .unwrap_or_else(|error| {
                    inputs
                        .notes
                        .push(format!("stylesheet failed to load: {error}"));
                    None
                })
                .unwrap_or_default(),
        };
        sheets.push((source, text));
    }
    let (stylesheet, dropped) = Stylesheet::parse_recovering(&joined_sheets(&sheets));
    inputs.notes.extend(
        dropped
            .into_iter()
            .map(|rule| format!("dropped CSS rule: {rule}")),
    );
    if stylesheet.font_faces() != inputs.stylesheet.font_faces() || fonts_changed {
        load_web_fonts(load, &inputs.fonts, base, &stylesheet, &policies);
        add_script_fonts(document, &mut inputs);
    }
    inputs.stylesheet = Rc::new(stylesheet);
    inputs.sheets = sheets;
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
// the current frame of each animated image the layout shows, in place of its first frame
fn animated_layers(page: &Page, layout: &LayoutDocument) -> Vec<ImageLayer> {
    let mut animated = page.animated.borrow_mut();
    if animated.is_empty() {
        return Vec::new();
    }
    let now = page.runtime.now();
    for animation in animated.iter() {
        animation.shown.set(false);
    }
    let mut layers = Vec::new();
    for layer in &layout.images {
        if let Some(animation) = animated
            .iter_mut()
            .find(|animation| Arc::ptr_eq(&animation.first, &layer.image))
        {
            animation.shown.set(true);
            layers.push(ImageLayer {
                node: layer.node,
                image: animation.frame_at(now),
                background: layer.background,
            });
        }
    }
    layers
}

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
    let path = decode_path(path, false)
        .map_err(|error| NavigationError::InvalidResourcePath(format!("{path}: {error}")))?;
    Ok(if path == "/" {
        "/index.html".to_string()
    } else {
        path
    })
}

// a files path holds a file's own name, where an escaped colon is allowed outside windows
fn decode_path(raw_path: &str, files: bool) -> Result<String, NavigationError> {
    let path = decode_path_bytes(raw_path, files)?;
    validate_path(&path)?;
    Ok(path)
}

fn resolve_reference_path(raw_path: &str, files: bool) -> Result<String, NavigationError> {
    let path = decode_path_bytes(raw_path, files)?;
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

fn decode_path_bytes(raw_path: &str, files: bool) -> Result<String, NavigationError> {
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
            if value != b':' {
                reject_path_byte(value, true)?;
            }
            decoded.push(value);
            cursor += 3;
            continue;
        }
        // an app path may hold a colon, as vite's /@fs/D:/ paths do; the packaged files
        // refuse one on their own, and a files path names a colon escaped
        if files || byte != b':' {
            reject_path_byte(byte, false)?;
        }
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
        "mp4" | "m4v" => "video/mp4",
        "m4a" => "audio/mp4",
        "mp3" => "audio/mpeg",
        "aac" => "audio/aac",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "ogv" => "video/ogg",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
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
    fn engine_made_objects_belong_to_their_interfaces() {
        let mut browser = Browser::new(bundle(
            r#"<canvas id="c"></canvas><input id="f" type="file"><p id='status'>idle</p><script>
              const context = document.getElementById("c").getContext("2d");
              const checks = [
                history instanceof History, navigator instanceof Navigator,
                performance instanceof Performance, performance instanceof EventTarget,
                matchMedia("(min-width: 1px)") instanceof MediaQueryList,
                document.getElementById("f").files instanceof FileList,
                context instanceof CanvasRenderingContext2D,
                context.measureText("a") instanceof TextMetrics,
                indexedDB instanceof IDBFactory, navigator.locks instanceof LockManager,
                document.createTreeWalker(document.body) instanceof TreeWalker,
                document.createNodeIterator(document.body) instanceof NodeIterator,
                Object.prototype.toString.call(history),
              ];
              try { new History(); } catch (error) { checks.push(error.name); }
              const stored = new StorageEvent("storage", { key: "k", newValue: "v" });
              checks.push(stored.key, stored.oldValue, stored.newValue, stored.url === "");
              checks.push(new FormDataEvent("formdata", { formData: new FormData() }).formData instanceof FormData);
              navigator.locks.request("x", (lock) => {
                checks.push(lock instanceof Lock);
                requestIdleCallback((deadline) => {
                  checks.push(deadline instanceof IdleDeadline);
                  document.getElementById("status").textContent = checks.join("|");
                });
              });
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/index.html")
            .expect("packaged document should load");
        browser.advance_time(100).expect("time should advance");
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some(
                "true|true|true|true|true|true|true|true|true|true|true|true|[object History]|\
                 TypeError|k||v|true|true|true|true"
            )
        );
    }

    #[test]
    fn declarative_shadow_roots_attach_on_load_and_through_set_html_unsafe() {
        let mut browser = Browser::new(bundle(
            r##"<div id="host"><template shadowrootmode="open"><b>inside</b><slot></slot></template>light</div>
            <div id="later"></div><div id="plain"></div><p id='status'>idle</p><script>
              const host = document.getElementById("host");
              const later = document.getElementById("later");
              later.setHTMLUnsafe('<span id="s"><template shadowrootmode="closed"><i>hidden</i></template></span>');
              document.getElementById("plain").innerHTML = '<span><template shadowrootmode="open"></template></span>';
              const span = later.querySelector("#s");
              host.shadowRoot.setHTMLUnsafe("<em>swapped</em>");
              document.getElementById("status").textContent = [
                host.shadowRoot.mode, host.shadowRoot.innerHTML, host.textContent.trim(),
                span.shadowRoot, span.querySelector("template"),
                document.querySelector("#plain template") !== null,
              ].join("|");
            </script>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/index.html")
            .expect("packaged document should load");
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("open|<em>swapped</em>|light|||true")
        );
    }

    #[test]
    fn a_viewport_that_changes_size_fires_resize_at_the_next_rendering_step() {
        let mut browser = Browser::new(bundle(
            r#"<p id='status'>idle</p><script>
              const seen = [];
              addEventListener("resize", () => seen.push("window:" + innerWidth));
              visualViewport.addEventListener("resize", () => {
                seen.push("visual:" + visualViewport.width + "x" + visualViewport.height);
                document.getElementById("status").textContent = seen.join("|");
              });
              seen.push(visualViewport instanceof VisualViewport, visualViewport instanceof EventTarget, visualViewport.scale);
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
            .navigate("https://app.ceri.invalid/index.html")
            .expect("packaged document should load");
        browser.advance_time(16).expect("first rendering step");
        browser
            .advance_time(16)
            .expect("a quiet step fires nothing");
        browser.set_viewport(
            Size {
                width: 500.0,
                height: 320.0,
            },
            1.0,
        );
        browser.advance_time(16).expect("the step after a resize");
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("true|true|1|window:500|visual:500x320")
        );
    }

    #[test]
    fn view_transitions_run_the_update_and_settle_in_order() {
        let mut browser = Browser::new(bundle(
            r#"<p id='status'>idle</p><script>
              const seen = [];
              const status = document.getElementById("status");
              const first = document.startViewTransition(async () => { seen.push("update"); status.dataset.step = "1"; });
              seen.push(first instanceof ViewTransition, document.activeViewTransition === first, "started");
              first.updateCallbackDone.then(() => seen.push("done"));
              first.ready.then(() => seen.push("ready"));
              first.finished.then(() => {
                seen.push("finished", document.activeViewTransition);
                const skipped = document.startViewTransition(() => seen.push("second update"));
                skipped.skipTransition();
                skipped.ready.catch((error) => seen.push(error.name));
                skipped.finished.then(() => {
                  const failing = document.startViewTransition({ update() { throw new RangeError("no"); } });
                  failing.ready.catch(() => {});
                  failing.finished.catch(() => {});
                  failing.updateCallbackDone.catch((error) => {
                    seen.push(error.name);
                    status.textContent = seen.join("|");
                  });
                });
              });
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/index.html")
            .expect("packaged document should load");
        for _ in 0..4 {
            browser.advance_time(16).expect("rendering step");
        }
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some(
                "true|true|started|update|done|ready|finished||AbortError|second update|RangeError"
            )
        );
    }

    #[test]
    fn svg_bounding_boxes_are_in_each_element_s_own_user_space() {
        let mut browser = Browser::new(bundle(
            r##"<svg id="s" width="100" height="100" viewBox="0 0 50 50"><g id="g" transform="translate(5 5)">
              <rect id="r" x="10" y="10" width="20" height="5" transform="scale(2)"/>
              <path id="p" d="M0 0 C 0 20 20 20 20 0"/><use id="u" href="#r" x="1"/></g>
              <defs><linearGradient id="l"></linearGradient></defs></svg>
            <p id='status'>idle</p><script>
              const box = (id) => { const b = document.getElementById(id).getBBox(); return [b.x, b.y, b.width, b.height].join(","); };
              document.getElementById("status").textContent = [
                box("r"), box("p"), box("u"), box("g"), box("s"),
                document.getElementById("s") instanceof SVGSVGElement,
                document.getElementById("r") instanceof SVGGraphicsElement,
                document.getElementById("l") instanceof SVGGraphicsElement,
                document.getElementById("r").getBBox() instanceof DOMRect,
              ].join("|");
            </script>"##,
        ));
        browser.set_viewport(
            Size {
                width: 400.0,
                height: 300.0,
            },
            1.0,
        );
        browser
            .navigate("https://app.ceri.invalid/index.html")
            .expect("packaged document should load");
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("10,10,20,5|0,0,20,15|21,20,40,10|0,0,61,30|5,5,61,30|true|true|false|true")
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
        for _ in 0..5000 {
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
        for _ in 0..5000 {
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
        for _ in 0..5000 {
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
        for _ in 0..5000 {
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

    fn element_text(browser: &Browser, id: &str) -> String {
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let node = document
            .find_element_by_id(id)
            .expect("element should exist");
        document.text_content(node)
    }

    #[test]
    fn a_meta_policy_refuses_what_it_does_not_list_and_logs_each_refusal() {
        let mut browser = Browser::new(bundle(
            r#"<meta http-equiv="Content-Security-Policy" content="default-src 'self'; script-src 'self' 'nonce-abc'">
            <p id="out">idle</p><p id="box" style="color: red"></p>
            <button id="press" onclick="document.getElementById('out').textContent = 'pressed'">press</button>
            <img src="https://files.ceri.invalid/a.png" width="4" height="4">
            <script>document.getElementById("out").textContent = "inline ran";</script>
            <script nonce="abc">
                const notes = [];
                try { new Function("return 1"); notes.push("function ran"); } catch (error) { notes.push(error.name); }
                fetch("https://example.com/feed").then(
                    () => notes.push("fetched"),
                    (error) => notes.push(error.name),
                ).then(() => { document.getElementById("out").textContent = notes.join(","); });
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        browser.pump().expect("the fetch should settle");
        assert!(browser.click_id("press").expect("click should dispatch"));
        assert_eq!(element_text(&browser, "out"), "EvalError,TypeError");
        let document = browser.document().expect("document should exist");
        let boxed = document.borrow().find_element_by_id("box").expect("box");
        assert!(document.borrow().style_blocked(boxed));
        browser
            .paint(Size {
                width: 100.0,
                height: 60.0,
            })
            .expect("page should paint");
        let logs = browser.console_logs().to_vec();
        let script = "\"script-src 'self' 'nonce-abc'\".";
        let has = |prefix: &str| logs.iter().any(|line| line.starts_with(prefix));
        assert!(
            has(&format!(
                "Refused to execute inline script because it violates the following Content \
                 Security Policy directive: {script} Either the 'unsafe-inline' keyword, a hash \
                 ('sha256-"
            )),
            "{logs:?}"
        );
        assert!(
            has(&format!(
                "Refused to execute inline event handler because it violates the following \
                 Content Security Policy directive: {script}"
            )),
            "{logs:?}"
        );
        assert!(
            has(
                "Refused to apply inline style because it violates the following Content \
                 Security Policy directive: \"default-src 'self'\"."
            ),
            "{logs:?}"
        );
        for line in [
            format!(
                "Refused to evaluate a string as JavaScript because 'unsafe-eval' is not an \
                 allowed source of script in the following Content Security Policy directive: \
                 {script}"
            ),
            "Refused to connect to 'https://example.com/feed' because it violates the following \
             Content Security Policy directive: \"default-src 'self'\". Note that 'connect-src' \
             was not explicitly set, so 'default-src' is used as a fallback."
                .to_string(),
            "Refused to load the image 'https://files.ceri.invalid/a.png' because it violates the \
             following Content Security Policy directive: \"default-src 'self'\". Note that \
             'img-src' was not explicitly set, so 'default-src' is used as a fallback."
                .to_string(),
        ] {
            assert_eq!(
                logs.iter().filter(|logged| **logged == line).count(),
                1,
                "{line} in {logs:?}"
            );
        }
    }

    #[test]
    fn an_app_policy_runs_bundled_inline_code_and_still_refuses_eval() {
        let mut browser = Browser::new(bundle(
            r#"<p id="out">idle</p><script>
                let note;
                try { new Function("return 1"); note = "ran"; } catch (error) { note = error.name; }
                document.getElementById("out").textContent = note;
            </script>"#,
        ))
        .with_content_security_policy("default-src 'self'");
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        assert_eq!(element_text(&browser, "out"), "EvalError");
        assert_eq!(
            browser.console_logs(),
            [
                "Refused to evaluate a string as JavaScript because 'unsafe-eval' is not an allowed \
              source of script in the following Content Security Policy directive: \
              \"default-src 'self'\"."
            ]
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
    fn location_reload_loads_a_fresh_document_and_keeps_the_entry_state() {
        let mut browser = Browser::new(bundle(
            r#"<button id="go">reload</button><p id="status"></p><script>
                document.getElementById("status").textContent = JSON.stringify(history.state);
                document.getElementById("go").addEventListener("click", () => {
                    history.replaceState({ draft: 6 }, "");
                    history.pushState({ draft: 7 }, "");
                    location.reload();
                });
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("home should load");
        let first = browser.document().expect("home document should exist");
        assert!(browser.click_id("go").expect("reload should navigate"));
        let second = browser.document().expect("reloaded document should exist");
        assert!(!Rc::ptr_eq(&first, &second));
        assert_eq!(browser.history().len(), 2);
        let status = second
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            second.borrow().element_text_content(status).as_deref(),
            Some(r#"{"draft":7}"#)
        );
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
    fn seeded_local_storage_reaches_pages_once() {
        let storage_root =
            std::env::temp_dir().join(format!("ceri-engine-storage-seed-{}", std::process::id()));
        let seed = |value: &str| {
            let value = value.to_string();
            crate::script::seed_local_storage(&storage_root, move || {
                Ok(vec![("view".to_string(), value)])
            })
            .expect("seeding should work")
        };
        assert!(seed("{\"zoom\":3}"));
        assert!(!seed("{\"zoom\":1}"));
        let mut browser = Browser::new(bundle(
            r#"<p id="out"></p><script>
                document.getElementById("out").textContent = localStorage.getItem("view");
            </script>"#,
        ))
        .with_storage_directory(&storage_root)
        .expect("seeded storage should open");
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        assert_eq!(element_text(&browser, "out"), "{\"zoom\":3}");
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
    fn exec_command_edits_text_controls_and_copies_after_a_press() {
        let text = Rc::new(RefCell::new(String::new()));
        let writes = Rc::new(RefCell::new(Vec::new()));
        let provider = Rc::new(MemoryClipboard {
            text: text.clone(),
            writes: writes.clone(),
        });
        let mut browser = Browser::new(bundle(
            r#"<input id="field" value="abc"><button id="copy">copy</button><p id="status">idle</p><script>
                const field = document.getElementById("field");
                const seen = [];
                field.addEventListener("input", () => seen.push("input:" + field.value));
                field.focus();
                field.setSelectionRange(3, 3);
                seen.push(document.execCommand("insertText", false, "de"));
                seen.push(document.execCommand("delete"), field.value);
                field.setSelectionRange(0, 0);
                seen.push(document.execCommand("forwardDelete"), field.value);
                seen.push(document.queryCommandSupported("copy"), document.queryCommandEnabled("copy"));
                seen.push(document.execCommand("copy"), document.execCommand("bold"));
                document.getElementById("copy").addEventListener("click", () => {
                    field.focus();
                    document.execCommand("selectAll");
                    seen.push(document.execCommand("copy"));
                    document.getElementById("status").textContent = seen.join("|");
                });
            </script>"#,
        ))
        .with_clipboard_provider(provider);
        browser
            .navigate("https://app.ceri.invalid/index.html")
            .expect("packaged document should load");
        browser.click_id("copy").expect("click should run");
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some(
                "input:abcde|true|input:abcd|true|abcd|input:bcd|true|bcd|true|false|false|false|true"
            )
        );
        assert_eq!(writes.borrow().as_slice(), ["bcd"]);
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
    fn clipboard_items_write_pictures_and_text_and_read_back() {
        #[derive(Default)]
        struct Board {
            text: RefCell<String>,
            image: RefCell<Option<(String, Vec<u8>)>>,
        }
        struct Shared(Rc<Board>);
        impl ClipboardProvider for Shared {
            fn read_text(&self) -> Result<String, NavigationError> {
                Ok(self.0.text.borrow().clone())
            }
            fn write_text(&self, text: &str) -> Result<(), NavigationError> {
                *self.0.text.borrow_mut() = text.to_string();
                *self.0.image.borrow_mut() = None;
                Ok(())
            }
            fn read_image(&self) -> Result<Option<(String, Vec<u8>)>, NavigationError> {
                Ok(self.0.image.borrow().clone())
            }
            fn write_image(&self, mime_type: &str, bytes: &[u8]) -> Result<(), NavigationError> {
                *self.0.image.borrow_mut() = Some((mime_type.to_string(), bytes.to_vec()));
                Ok(())
            }
        }
        let board = Rc::new(Board::default());
        let mut browser = Browser::new(bundle(
            r#"<p id="status">idle</p><script>
                const seen = [];
                const show = () => (document.getElementById("status").textContent = seen.join("|"));
                const later = new Promise((resolve) => setTimeout(() => resolve(new Blob([new Uint8Array([7, 8])], { type: "image/png" })), 5));
                const item = new ClipboardItem({ "image/png": later, "text/plain": "caption" });
                seen.push(item.types.join(","));
                navigator.clipboard.write([item])
                    .then(() => navigator.clipboard.read())
                    .then((items) => {
                        seen.push(items.length, items[0].types.join(","));
                        return items[0].getType("image/png");
                    })
                    .then((blob) => blob.arrayBuffer())
                    .then((buffer) => {
                        seen.push(new Uint8Array(buffer).join(","));
                        return navigator.clipboard.write([new ClipboardItem({ "text/plain": "words" })]);
                    })
                    .then(() => navigator.clipboard.readText())
                    .then((text) => {
                        seen.push(text);
                        return navigator.clipboard.write([]);
                    })
                    .catch((error) => seen.push(error.name))
                    .then(() => {
                        try { new ClipboardItem({}); } catch (error) { seen.push(error.name); }
                        show();
                    });
            </script>"#,
        ))
        .with_clipboard_provider(Rc::new(Shared(board.clone())));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        browser.advance_time(10).expect("time should pass");
        assert_eq!(
            element_text(&browser, "status"),
            "image/png,text/plain|1|image/png|7,8|words|DataError|TypeError"
        );
        assert_eq!(board.text.borrow().as_str(), "words");
    }

    #[test]
    fn element_animate_moves_a_box_finishes_and_cancels() {
        let mut browser = Browser::new(bundle(
            r#"<style>body { margin: 0 } #box { width: 10px; height: 10px }</style>
            <div id="box"></div><button id="go">go</button><p id="status">idle</p><script>
                const box = document.getElementById("box");
                const seen = [];
                const show = () => (document.getElementById("status").textContent = seen.join("|"));
                const grow = box.animate([{ width: "10px" }, { width: "110px" }], { duration: 100, fill: "forwards" });
                seen.push(grow.playState, grow.currentTime, document.getAnimations().length,
                    box.getAnimations()[0] === grow, grow instanceof Animation, String(grow));
                grow.onfinish = (event) => (seen.push("finish:" + event.type + ":" + grow.playState + ":" + grow.currentTime), show());
                grow.finished.then((done) => (seen.push("promise:" + (done === grow)), show()));
                const fade = box.animate({ opacity: [0, 1] }, 1000);
                fade.oncancel = () => seen.push("cancel");
                fade.finished.catch((error) => (seen.push("rejected:" + error.name), show()));
                document.getElementById("go").addEventListener("click", () => {
                    fade.pause();
                    seen.push(fade.playState, fade.currentTime);
                    fade.cancel();
                    seen.push(fade.playState, fade.currentTime, document.getAnimations().length);
                    show();
                });
                show();
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let viewport = Size {
            width: 300.0,
            height: 200.0,
        };
        let width = |browser: &Browser| {
            let layout = browser.layout(viewport).expect("layout");
            find_box(&layout.root, "div").expect("box").rect.width
        };
        assert_eq!(width(&browser), 10.0);
        assert_eq!(
            element_text(&browser, "status"),
            "running|0|1|true|true|[object Animation]"
        );
        browser.advance_time(50).expect("time should pass");
        assert_eq!(width(&browser), 60.0);
        browser.advance_time(60).expect("time should pass");
        // held at its last keyframe, as fill forwards asks
        assert_eq!(width(&browser), 110.0);
        assert_eq!(
            element_text(&browser, "status"),
            "running|0|1|true|true|[object Animation]|finish:finish:finished:100|promise:true"
        );
        browser
            .click_id("go")
            .expect("the button should take the click");
        browser.advance_time(1).expect("time should pass");
        assert!(
            element_text(&browser, "status")
                .ends_with("|paused|110|cancel|idle||1|rejected:AbortError"),
            "{}",
            element_text(&browser, "status")
        );
    }

    #[test]
    fn screen_reports_the_display_or_the_viewport_until_one_is_known() {
        let mut browser = Browser::new(bundle(
            r#"<button id="go">read</button><p id="status">idle</p><script>
                window.read = () => {
                    const s = screen;
                    document.getElementById("status").textContent = [
                        s.width, s.height, s.availLeft, s.availTop, s.availWidth, s.availHeight,
                        s.colorDepth, s.orientation.type, s === screen, window.screen === s,
                    ].join("|");
                };
                read();
                document.getElementById("go").addEventListener("click", read);
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
            .expect("page should load");
        browser
            .click_id("go")
            .expect("the button should take the click");
        assert_eq!(
            element_text(&browser, "status"),
            "400|300|0|0|400|300|24|landscape-primary|true|true"
        );
        browser.set_display((1080.0, 1920.0), [0.0, 0.0, 1080.0, 1872.0]);
        browser
            .click_id("go")
            .expect("the button should take the click");
        assert_eq!(
            element_text(&browser, "status"),
            "1080|1920|0|0|1080|1872|24|portrait-primary|true|true"
        );
    }

    #[test]
    fn the_clipboard_keys_fire_their_events_and_a_handler_can_take_them_over() {
        struct PictureClipboard {
            text: Rc<RefCell<String>>,
        }
        impl ClipboardProvider for PictureClipboard {
            fn read_text(&self) -> Result<String, NavigationError> {
                Ok(self.text.borrow().clone())
            }
            fn write_text(&self, text: &str) -> Result<(), NavigationError> {
                *self.text.borrow_mut() = text.to_string();
                Ok(())
            }
            fn read_image(&self) -> Result<Option<(String, Vec<u8>)>, NavigationError> {
                Ok(Some(("image/png".to_string(), vec![1, 2, 3])))
            }
        }
        let text = Rc::new(RefCell::new("from outside".to_string()));
        let mut browser = Browser::new(bundle(
            r#"<input id="field" value="kept"><p id="status">idle</p><p id="value"></p><script>
                const seen = [];
                const show = () => (document.getElementById("status").textContent = seen.join("|"));
                let take = true;
                addEventListener("paste", (event) => {
                    const data = event.clipboardData;
                    const file = data.items[1].getAsFile();
                    let heard = "";
                    data.items[0].getAsString((value) => (heard = value));
                    seen.push(event.constructor.name, event.bubbles && event.cancelable,
                        data.getData("text"), data.types.join(","), data.items.length,
                        data.items[1].kind + ":" + data.items[1].type, file.name + ":" + file.size,
                        data.files.length, heard, data.items[0].getAsFile());
                    data.setData("text/plain", "ignored");
                    seen.push(data.getData("text/plain"));
                    if (take) event.preventDefault();
                    take = false;
                    show();
                });
                addEventListener("copy", (event) => {
                    event.clipboardData.setData("text/plain", "set by the page");
                    event.preventDefault();
                    seen.push("copy");
                    show();
                });
                addEventListener("cut", () => (seen.push("cut seen"), show()));
                const field = document.getElementById("field");
                field.addEventListener("input", () => {
                    document.getElementById("value").textContent = field.value;
                });
                field.focus();
            </script>"#,
        ))
        .with_clipboard_provider(Rc::new(PictureClipboard { text: text.clone() }));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let control = KeyModifiers {
            control: true,
            ..KeyModifiers::default()
        };
        assert!(browser.key_down_with_modifiers("v", control).unwrap());
        assert_eq!(
            element_text(&browser, "status"),
            "ClipboardEvent|true|from outside|text/plain,Files|2|file:image/png|image.png:3|1|from outside||from outside"
        );
        // the handler took the paste, so the field kept its text; the copy it took writes
        // what it set
        assert!(browser.key_down_with_modifiers("c", control).unwrap());
        assert_eq!(text.borrow().as_str(), "set by the page");
        assert!(element_text(&browser, "status").ends_with("|copy"));
        // left alone, a paste goes into the field as before
        assert!(browser.key_down_with_modifiers("v", control).unwrap());
        assert_eq!(element_text(&browser, "value"), "keptset by the page");
    }

    #[test]
    fn computed_style_follows_custom_properties_and_disabled_state_written_between_reads() {
        let mut browser = Browser::new(bundle(
            r#"<style>:root { --tint: rgb(1, 2, 3) } input { color: var(--tint) } input:disabled { color: rgb(9, 9, 9) }</style><fieldset id="set"><input id="field"></fieldset><p id="status">idle</p><script>
                const field = document.getElementById("field");
                const read = () => getComputedStyle(field).color;
                const before = read();
                document.documentElement.style.setProperty("--tint", "rgb(4, 5, 6)");
                const tinted = read();
                document.getElementById("set").disabled = true;
                const disabled = read();
                document.getElementById("set").disabled = false;
                document.getElementById("status").textContent = [before, tinted, disabled, read()].join("|");
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("computed style page should load");
        assert_eq!(
            element_text(&browser, "status"),
            "rgb(1, 2, 3)|rgb(4, 5, 6)|rgb(9, 9, 9)|rgb(4, 5, 6)"
        );
    }

    #[test]
    fn computed_style_objects_made_alike_stay_apart() {
        let mut browser = Browser::new(bundle(
            r#"<style>#a { color: rgb(1, 2, 3) } #b { color: rgb(4, 5, 6) }</style><p id="a">a</p><p id="b">b</p><p id="status">idle</p><script>
                const first = getComputedStyle(document.getElementById("a"));
                const second = getComputedStyle(document.getElementById("b"));
                const third = getComputedStyle(document.getElementById("a"));
                second.extra = 1;
                delete third.color;
                document.getElementById("status").textContent = [
                    first.color, second.color, third.color, first.extra,
                    Object.keys(first).length === Object.keys(second).length - 1,
                ].join("|");
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("computed style page should load");
        assert_eq!(
            element_text(&browser, "status"),
            "rgb(1, 2, 3)|rgb(4, 5, 6)|||true"
        );
    }

    #[test]
    fn computed_style_resolves_times_urls_and_transforms_as_chrome_does() {
        let mut browser = Browser::new(bundle(
            r#"<style>#box { width: 100px; height: 40px; transition-duration: 1s, 250ms; animation-delay: 1500ms; background-image: url("a.png"), none; transform: translate(10px, 50%) rotate(45deg); } #flat { transform: none } #deep { transform: translateZ(5px) }</style><div id="box"></div><div id="flat"></div><div id="deep"></div><p id="status">idle</p><script>
                const style = (id) => getComputedStyle(document.getElementById(id));
                document.getElementById("status").textContent = [
                    style("box").transitionDuration, style("box").animationDelay, style("box").backgroundImage,
                    style("box").transform, style("flat").transform, style("deep").transform,
                ].join("|");
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("computed style page should load");
        assert_eq!(
            element_text(&browser, "status"),
            "1s, 0.25s|1.5s|url(\"https://app.ceri.invalid/a.png\"), none|matrix(0.707107, 0.707107, -0.707107, 0.707107, 10, 20)|none|matrix3d(1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 5, 1)"
        );
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
    fn paint_only_motion_keeps_the_last_layout_and_matches_a_full_one() {
        let viewport = Size {
            width: 200.0,
            height: 120.0,
        };
        let page = |rules: &str| {
            let mut browser = Browser::new(bundle(&format!(
                "<style>@keyframes glow {{ 50% {{ box-shadow: 0 0 0 4px #0000ff; border-color: #00ff00; background-color: #ffff00 }} }} #dot {{ width: 20px; height: 20px; border: 2px solid #000000; box-shadow: 0 0 0 4px #ff0000 }} {rules}</style><div id=\"dot\"></div><p>text</p>"
            )));
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("motion page should load");
            browser.layout(viewport).expect("first layout");
            browser
        };
        let mut browser = page("#dot { animation: glow 1000ms linear infinite }");
        for _ in 0..3 {
            // a mark a full layout would not keep
            let marked = {
                let mut cache = browser.layout_cache.borrow_mut();
                let cached = cache.as_mut().expect("a cached layout");
                assert!(cached.motion.is_some());
                let mut layout = (*cached.layout).clone();
                layout.root.tag = "#marked".to_string();
                cached.layout = Rc::new(layout);
                cached.layout.root.clone()
            };
            browser.advance_time(120).expect("animation frame");
            let mut fast = (*browser.layout(viewport).expect("fast layout")).clone();
            assert_eq!(fast.root.tag, "#marked");
            assert_ne!(fast.root, marked);
            *browser.layout_cache.borrow_mut() = None;
            let full = browser.layout(viewport).expect("full layout");
            fast.root.tag = full.root.tag.clone();
            assert_eq!(fast, *full);
        }
        let browser =
            page("@keyframes grow { to { width: 40px } } #dot { animation: grow 1000ms infinite }");
        let cache = browser.layout_cache.borrow();
        assert!(cache.as_ref().expect("a cached layout").motion.is_none());
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
    fn intersection_observers_all_read_one_layout_before_their_callbacks() {
        let mut browser = Browser::new(bundle(
            r#"<style>body { margin: 0 } div { height: 10px }</style><div id="one"></div><div id="two"></div><div id="three"></div><p id="status">idle</p><script>
                globalThis.log = [];
                const watch = (id, callback) => {
                    const observer = new IntersectionObserver(callback);
                    observer.observe(document.getElementById(id));
                    return observer;
                };
                let third;
                watch("one", () => {
                    log.push("one");
                    const tall = document.createElement("section");
                    tall.style.height = "5000px";
                    document.body.prepend(tall);
                    third.disconnect();
                });
                watch("two", (entries) => log.push("two:" + entries[0].isIntersecting));
                third = watch("three", () => log.push("three"));
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
        browser.advance_time(16).expect("rendering step");
        let page = browser.page.as_mut().expect("page should be loaded");
        page.runtime
            .execute("report();")
            .expect("report should run");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some("one,two:true")
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

    // ten frames a second for a second, each four by two and as red as ten times its number
    struct Reel;

    struct ReelReader(usize);

    impl crate::MediaDecoder for Reel {
        fn open(&self, source: &crate::MediaSource) -> Result<Box<dyn crate::VideoReader>, String> {
            match source {
                crate::MediaSource::Bytes { body, .. } if &body[..] == b"reel" => {
                    Ok(Box::new(ReelReader(0)))
                }
                _ => Err("not a reel".into()),
            }
        }

        fn can_play(&self, mime_type: &str) -> &'static str {
            if mime_type == "video/mp4" {
                "maybe"
            } else {
                ""
            }
        }
    }

    impl crate::VideoReader for ReelReader {
        fn info(&self) -> crate::MediaInfo {
            crate::MediaInfo {
                duration: 1.0,
                width: 4,
                height: 2,
            }
        }

        fn seek(&mut self, seconds: f64) -> Result<(), String> {
            self.0 = (seconds * 10.0).floor() as usize;
            Ok(())
        }

        fn read(&mut self) -> Result<Option<crate::VideoFrame>, String> {
            if self.0 == 10 {
                return Ok(None);
            }
            let red = (self.0 * 20) as u8;
            let frame = crate::VideoFrame {
                time: self.0 as f64 / 10.0,
                width: 4,
                height: 2,
                bgra: [0, 0, red, 0].repeat(8),
                surface: None,
            };
            self.0 += 1;
            Ok(Some(frame))
        }
    }

    // the reel as a gpu decoder gives it, each frame a surface that holds its rows
    struct SurfaceReel;

    struct SurfaceReader(Box<dyn crate::VideoReader>);

    struct Rows(Vec<u8>);

    impl crate::VideoSurface for Rows {
        fn shared_handle(&self) -> isize {
            0
        }

        fn read(&self) -> Result<Vec<u8>, String> {
            Ok(self.0.clone())
        }
    }

    impl crate::MediaDecoder for SurfaceReel {
        fn open(&self, source: &crate::MediaSource) -> Result<Box<dyn crate::VideoReader>, String> {
            Ok(Box::new(SurfaceReader(Reel.open(source)?)))
        }
    }

    impl crate::VideoReader for SurfaceReader {
        fn info(&self) -> crate::MediaInfo {
            self.0.info()
        }

        fn seek(&mut self, seconds: f64) -> Result<(), String> {
            self.0.seek(seconds)
        }

        fn read(&mut self) -> Result<Option<crate::VideoFrame>, String> {
            Ok(self.0.read()?.map(|mut frame| {
                frame.surface = Some(Arc::new(Rows(std::mem::take(&mut frame.bgra))));
                frame
            }))
        }
    }

    // runs a page whose video frames come from another thread until its status is set
    fn play_until_status(browser: &mut Browser) -> String {
        for _ in 0..2000 {
            browser.pump().expect("media should pump");
            browser.advance_time(16).expect("tasks should run");
            let document = browser.document().expect("document");
            let document = document.borrow();
            let status = document.find_element_by_id("status").expect("status");
            let text = document.element_text_content(status).unwrap_or_default();
            if !text.is_empty() {
                return text;
            }
            drop(document);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the page never reported");
    }

    #[test]
    fn an_indeterminate_checkbox_paints_a_dash_until_it_is_clicked() {
        let resources = bundle(
            r#"<style>body { margin: 0 } input { margin: 0; width: 20px; height: 20px; display: block } input:indeterminate { accent-color: rgb(0, 200, 0) }</style><input id="box" type="checkbox"><script>document.getElementById("box").indeterminate = true;</script>"#,
        );
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("checkbox page should load");
        let size = Size {
            width: 60.0,
            height: 40.0,
        };
        let canvas = browser.paint(size).expect("page should paint");
        assert_eq!(pixel_at(&canvas, 10, 10), [255, 255, 255, 255], "the dash");
        assert_eq!(
            pixel_at(&canvas, 10, 4),
            [0, 200, 0, 255],
            "filled, as :indeterminate"
        );
        browser.click_id("box").expect("click should run");
        let canvas = browser.paint(size).expect("page should paint");
        assert_eq!(
            pixel_at(&canvas, 10, 4),
            [0, 117, 255, 255],
            "checked, no longer indeterminate"
        );
        assert_ne!(
            pixel_at(&canvas, 6, 10),
            [255, 255, 255, 255],
            "the dash is gone"
        );
    }

    #[test]
    fn a_video_plays_its_frames_to_the_end_by_the_page_clock() {
        let mut resources = bundle(
            r#"<video id="v" src="/reel.mp4" muted style="display: block; width: 40px; height: 20px"></video><p id="status"></p><script>
                const log = [];
                const v = document.getElementById("v");
                log.push(v instanceof HTMLVideoElement, v.paused, v.readyState, isNaN(v.duration), v.canPlayType("video/mp4"), v.muted);
                for (const name of ["loadstart", "durationchange", "loadedmetadata", "loadeddata", "canplay", "ratechange", "play", "playing", "pause", "ended", "emptied"]) {
                    v.addEventListener(name, () => log.push(name + "@" + v.currentTime.toFixed(1)));
                }
                v.addEventListener("loadeddata", () => {
                    log.push(v.videoWidth + "x" + v.videoHeight, v.duration, v.readyState, v.networkState);
                    v.playbackRate = 2;
                    v.play().then(() => log.push("played"));
                }, { once: true });
                v.onended = () => {
                    log.push(v.paused + ":" + v.ended);
                    document.getElementById("status").textContent = log.join(",");
                };
            </script>"#,
        );
        resources
            .insert("/reel.mp4", "video/mp4", b"reel".to_vec())
            .expect("video should be accepted");
        let mut browser = Browser::new(resources).with_media_decoder(Arc::new(Reel));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("video page should load");
        assert_eq!(
            play_until_status(&mut browser),
            "true,true,0,true,maybe,true,loadstart@0.0,durationchange@0.0,loadedmetadata@0.0,loadeddata@0.0,4x2,1,4,1,canplay@0.0,ratechange@0.0,play@0.0,playing@0.0,played,pause@1.0,ended@1.0,true:true"
        );
        let canvas = browser
            .paint(Size {
                width: 100.0,
                height: 60.0,
            })
            .expect("page should paint");
        assert_eq!(pixel_at(&canvas, 20, 10), [180, 0, 0, 255]);
    }

    #[test]
    fn a_playing_video_shown_as_a_plane_leaves_its_box_clear_and_new_frames_unpainted() {
        let mut resources = bundle(
            r#"<video id="v" src="/reel.mp4" muted style="display: block; width: 40px; height: 20px"></video><div id="cover" style="position: absolute; left: 8px; top: 8px; width: 10px; height: 10px; background: rgba(0, 0, 255, 0.6)"></div><p id="status"></p><script>
                const v = document.getElementById("v");
                v.onplaying = () => document.getElementById("status").textContent = "playing";
                v.play();
            </script>"#,
        );
        resources
            .insert("/reel.mp4", "video/mp4", b"reel".to_vec())
            .expect("video should be accepted");
        let mut browser = Browser::new(resources).with_media_decoder(Arc::new(SurfaceReel));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("video page should load");
        assert_eq!(play_until_status(&mut browser), "playing");
        let viewport = Size {
            width: 100.0,
            height: 60.0,
        };
        let (canvas, planes) = browser
            .paint_planes(viewport, 1.0, true)
            .expect("page should paint");
        let [(plane, frame, Some(_))] = &planes[..] else {
            panic!("the video should be one plane, not {}", planes.len());
        };
        assert_eq!(
            (plane.size, plane.bounds, plane.origin, plane.scale),
            ((4, 2), [0, 0, 40, 20], (0.0, 0.0), (10.0, 10.0))
        );
        assert_eq!(pixel_at(&canvas, 30, 15), [0, 0, 0, 0]);
        assert_eq!(pixel_at(&canvas, 10, 10), [0, 0, 153, 153]);
        assert_eq!(pixel_at(&canvas, 60, 20)[3], 255);
        let mut next = None;
        for _ in 0..2000 {
            browser.pump().expect("media should pump");
            browser.advance_time(16).expect("tasks should run");
            let (again, planes) = browser
                .paint_planes(viewport, 1.0, true)
                .expect("page should paint");
            // frames decoded since the video became a plane leave their surfaces unread
            if planes
                .first()
                .is_some_and(|(_, image, _)| !Arc::ptr_eq(image, frame) && image.pixels.is_empty())
            {
                next = Some((again, planes));
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let (again, planes) = next.expect("a new frame should arrive");
        assert_eq!(again.revision(), canvas.revision());
        assert_eq!(planes[0].0, *plane);
        let document = browser.document().expect("document");
        let cover = document
            .borrow()
            .find_element_by_id("cover")
            .expect("cover");
        document.borrow_mut().set_attribute(
            cover,
            "style",
            "position: absolute; left: 8px; top: 8px; width: 10px; height: 10px; background: rgba(255, 0, 0, 0.6)".to_string(),
        );
        let (covered, planes) = browser
            .paint_planes(viewport, 1.0, true)
            .expect("page should paint");
        assert_ne!(covered.revision(), canvas.revision());
        assert_eq!(planes.len(), 1);
        assert_eq!(planes[0].0, *plane);
        assert_eq!(pixel_at(&covered, 10, 10), [153, 0, 0, 153]);
        assert_eq!(pixel_at(&covered, 30, 15), [0, 0, 0, 0]);
        let painted = browser.paint(viewport).expect("page should paint");
        assert_eq!(pixel_at(&painted, 30, 15)[3], 255);
        assert_ne!(pixel_at(&painted, 30, 15)[0], 0);
    }

    #[test]
    fn a_rounded_video_plane_gives_up_its_box_by_corner_coverage() {
        let mut resources = bundle(
            r#"<video id="v" src="/reel.mp4" muted style="display: block; width: 40px; height: 20px; border-radius: 10px"></video><p id="status"></p><script>
                const v = document.getElementById("v");
                v.onplaying = () => document.getElementById("status").textContent = "playing";
                v.play();
            </script>"#,
        );
        resources
            .insert("/reel.mp4", "video/mp4", b"reel".to_vec())
            .expect("video should be accepted");
        let mut browser = Browser::new(resources).with_media_decoder(Arc::new(Reel));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("video page should load");
        assert_eq!(play_until_status(&mut browser), "playing");
        let viewport = Size {
            width: 100.0,
            height: 60.0,
        };
        let (canvas, planes) = browser
            .paint_planes(viewport, 1.0, true)
            .expect("page should paint");
        assert_eq!(planes.len(), 1);
        assert_eq!(planes[0].0.bounds, [0, 0, 40, 20]);
        assert_eq!(pixel_at(&canvas, 20, 10), [0, 0, 0, 0]);
        assert_eq!(pixel_at(&canvas, 0, 0)[3], 255);
        let edge = pixel_at(&canvas, 2, 3)[3];
        assert!(0 < edge && edge < 255, "{edge}");
    }

    #[test]
    fn a_detached_video_seeks_to_the_frame_drawn_into_a_canvas() {
        let mut resources = bundle(
            r#"<p id="status"></p><script>
                const log = [];
                const v = document.createElement("video");
                v.preload = "auto";
                v.addEventListener("loadeddata", () => {
                    v.addEventListener("seeking", () => log.push("seeking:" + v.seeking + ":" + v.currentTime));
                    v.addEventListener("seeked", () => {
                        const canvas = document.createElement("canvas");
                        canvas.width = 4;
                        canvas.height = 2;
                        const ctx = canvas.getContext("2d");
                        ctx.drawImage(v, 0, 0);
                        log.push("seeked:" + v.seeking + ":" + v.currentTime, Array.from(ctx.getImageData(1, 1, 1, 1).data).join(" "));
                        v.removeAttribute("src");
                        v.load();
                        log.push("emptied:" + v.networkState + ":" + v.readyState + ":" + isNaN(v.duration));
                        const broken = document.createElement("video");
                        broken.onerror = () => broken.play().catch((error) => {
                            log.push("error:" + broken.error.code + ":" + broken.networkState + ":" + error.name);
                            document.getElementById("status").textContent = log.join(",");
                        });
                        broken.src = "/missing.mp4";
                    }, { once: true });
                    v.currentTime = 0.55;
                }, { once: true });
                v.src = "/reel.mp4";
            </script>"#,
        );
        resources
            .insert("/reel.mp4", "video/mp4", b"reel".to_vec())
            .expect("video should be accepted");
        let mut browser = Browser::new(resources).with_media_decoder(Arc::new(Reel));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("video page should load");
        assert_eq!(
            play_until_status(&mut browser),
            "seeking:true:0.55,seeked:false:0.55,100 0 0 255,emptied:0:0:true,error:4:3:NotSupportedError"
        );
    }

    #[test]
    fn an_audio_element_streams_a_wave_file_through_its_clock() {
        let mut resources = bundle(
            r#"<p id="status"></p><script>
                const log = [];
                const a = new Audio();
                log.push(a instanceof HTMLAudioElement, Audio.prototype === HTMLAudioElement.prototype, a.preload, a.videoWidth, a.canPlayType("audio/WAV; codecs=1"), a.canPlayType("audio/ogg"));
                Object.assign(a, { preload: "auto", src: "/tone.wav" });
                a.onloadedmetadata = () => {
                    log.push("meta:" + a.duration);
                    a.currentTime = 0.25;
                };
                a.onseeked = () => {
                    log.push("seeked:" + a.currentTime);
                    a.volume = 0.5;
                    a.playbackRate = 2;
                    a.play().then(() => log.push("played"));
                };
                a.onended = () => {
                    log.push("ended:" + a.currentTime + ":" + a.paused);
                    a.removeAttribute("src");
                    a.load();
                    log.push("released:" + a.networkState + ":" + isNaN(a.duration));
                    const broken = new Audio("/missing.wav");
                    broken.onerror = () => {
                        log.push("error:" + broken.error.code + ":" + broken.error.message);
                        document.getElementById("status").textContent = log.join(",");
                    };
                };
            </script>"#,
        );
        let mut wave = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0data\x40\x1f\0\0".to_vec();
        wave.extend(std::iter::repeat_n([0x00, 0x10], 4000).flatten());
        resources
            .insert("/tone.wav", "audio/wav", wave)
            .expect("sound should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("audio page should load");
        assert_eq!(
            play_until_status(&mut browser),
            "true,true,auto,,maybe,,meta:0.5,seeked:0.25,played,ended:0.5:true,released:0:true,error:4:MEDIA_ELEMENT_ERROR: Format error"
        );
    }

    #[test]
    fn images_load_from_a_task_with_natural_sizes_and_decode() {
        let mut resources = bundle(
            r#"<p id="status">idle</p><img id="parsed" src="/shot.png"><script>
                const log = [];
                const parsed = document.getElementById("parsed");
                parsed.onload = () => log.push("parsed:" + parsed.naturalWidth);
                const img = new Image(4);
                log.push([img instanceof HTMLImageElement, Image.prototype === HTMLImageElement.prototype, img.getAttribute("width"), img.complete].join(":"));
                img.crossOrigin = "anonymous";
                log.push(img.crossOrigin + ":" + img.decoding);
                img.src = "/shot.png";
                log.push("set:" + img.complete + ":" + img.naturalWidth);
                img.addEventListener("load", () => log.push("load:" + img.naturalWidth + "x" + img.naturalHeight + ":" + img.complete + ":" + img.currentSrc));
                img.decode().then(() => {
                    log.push("decoded:" + img.naturalWidth);
                    const again = new Image();
                    again.src = "/shot.png";
                    log.push("cached:" + again.complete + ":" + again.naturalWidth);
                    const missing = new Image();
                    missing.onerror = () => log.push("error:" + missing.complete + ":" + missing.naturalWidth);
                    missing.src = "/missing.png";
                    missing.decode().catch((error) => {
                        log.push(error.name);
                        document.getElementById("status").textContent = log.join(",");
                    });
                });
                const holder = document.createElement("div");
                holder.innerHTML = '<img src="/shot.png">';
                holder.firstChild.onload = () => log.push("inner");
                const replaced = new Image();
                replaced.src = "/shot.png";
                replaced.decode().catch((error) => log.push("replaced:" + error.name));
                replaced.removeAttribute("src");
                log.push("removed:" + replaced.complete);
            </script>"#,
        );
        let (mime_type, png) = crate::image::decode_data_url("data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAA0AAAAHCAIAAABcElBNAAAAJHRFWHRDb21tZW50AGFuY2lsbGFyeSBjaHVua3MgYXJlIHNraXBwZWQXJBj8AAAAQElEQVR42mNgYNAQZjVW47Kz5Pf0EQmJl4wvkstqVS6dodGwWrd7n9G0i+YLn9isYWSXNxVm5SaImNjleYlBzHx2erxCwNKhtwAAAEFJREFUPAQRC0g5KzdBxKA8O9NsQYnn0vqYVV3566c2bVkwdefqFfu27T588NyJMw/PXv9y6RH79beMWrvKiPEHAPIZMLzOFGjtAAAAAElFTkSuQmCC").expect("fixture");
        resources
            .insert("/shot.png", &mime_type, png)
            .expect("image should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("image page should load");
        for _ in 0..4 {
            browser.advance_time(0).expect("image tasks should run");
        }
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some(
                "true:true:4:true,anonymous:auto,set:false:0,removed:true,replaced:EncodingError,parsed:13,load:13x7:true:https://app.ceri.invalid/shot.png,decoded:13,cached:true:13,inner,error:true:0,EncodingError"
            )
        );
    }

    #[test]
    fn canvases_draw_loaded_images_and_refuse_broken_ones() {
        let mut resources = bundle(
            r#"<p id="status">idle</p><script>
                const canvas = document.createElement("canvas");
                canvas.width = 13;
                canvas.height = 7;
                const ctx = canvas.getContext("2d");
                const img = new Image();
                img.src = "/shot.png";
                ctx.drawImage(img, 0, 0);
                const before = ctx.getImageData(2, 3, 1, 1).data[3];
                img.onload = () => {
                    ctx.drawImage(img, 0, 0, 13, 7);
                    const missing = new Image();
                    missing.onerror = () => {
                        let refused = "drawn";
                        try { ctx.drawImage(missing, 0, 0); } catch (error) { refused = error.name; }
                        const pixel = Array.from(ctx.getImageData(2, 3, 1, 1).data).join(" ");
                        document.getElementById("status").textContent = [before, pixel, refused].join(",");
                    };
                    missing.src = "/missing.png";
                };
            </script>"#,
        );
        let (mime_type, png) = crate::image::decode_data_url("data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAA0AAAAHCAIAAABcElBNAAAAJHRFWHRDb21tZW50AGFuY2lsbGFyeSBjaHVua3MgYXJlIHNraXBwZWQXJBj8AAAAQElEQVR42mNgYNAQZjVW47Kz5Pf0EQmJl4wvkstqVS6dodGwWrd7n9G0i+YLn9isYWSXNxVm5SaImNjleYlBzHx2erxCwNKhtwAAAEFJREFUPAQRC0g5KzdBxKA8O9NsQYnn0vqYVV3566c2bVkwdefqFfu27T588NyJMw/PXv9y6RH79beMWrvKiPEHAPIZMLzOFGjtAAAAAElFTkSuQmCC").expect("fixture");
        let expected = crate::image::decode_image(&png, &mime_type)
            .expect("fixture should decode")
            .pixel(2, 3);
        resources
            .insert("/shot.png", &mime_type, png)
            .expect("image should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("canvas page should load");
        for _ in 0..4 {
            browser.advance_time(0).expect("image tasks should run");
        }
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(expected.alpha, 255);
        assert_eq!(
            document.element_text_content(status),
            Some(format!(
                "0,{} {} {} 255,InvalidStateError",
                expected.red, expected.green, expected.blue
            ))
        );
    }

    #[test]
    fn fullscreen_follows_requests_made_after_a_press_and_escape_leaves_it() {
        let mut browser = Browser::new(bundle(
            r#"<p id="status">idle</p><button id="go">go</button><script>
                const seen = [];
                const note = (...items) => { seen.push(...items); document.getElementById("status").textContent = seen.join(","); };
                document.addEventListener("fullscreenchange", () => note("change:" + (document.fullscreenElement ? document.fullscreenElement.tagName : "none")));
                document.documentElement.requestFullscreen().catch((error) => note(error.name));
                document.getElementById("go").addEventListener("click", () => {
                    document.documentElement.requestFullscreen().then(() => note("granted", document.fullscreen, document.fullscreenEnabled));
                });
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        assert!(
            !browser.fullscreen(),
            "a request without a press is refused"
        );
        browser.set_pointer_button(0, true);
        browser.click_id("go").expect("click should run");
        browser.set_pointer_button(0, false);
        assert!(browser.fullscreen());
        browser
            .key_down_with_modifiers("Escape", KeyModifiers::default())
            .expect("escape should run");
        assert!(!browser.fullscreen());
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some("TypeError,change:HTML,granted,true,true,change:none")
        );
    }

    #[test]
    fn rules_from_the_cssom_and_adopted_sheets_style_the_page() {
        let mut browser = Browser::new(bundle(
            r#"<style id="s"></style><p class="css-1" id="p">x</p><p class="made" id="q">y</p><x-card id="card"></x-card><p id="status">idle</p><script>
                const sheet = document.getElementById("s").sheet;
                sheet.insertRule(".css-1{color:rgb(0, 128, 0)}", sheet.cssRules.length);
                const made = new CSSStyleSheet();
                made.replaceSync(".made { color: rgb(0, 0, 255) }");
                document.adoptedStyleSheets = [made];
                const root = document.getElementById("card").attachShadow({ mode: "open" });
                root.innerHTML = '<span id="inside">z</span>';
                const shadowSheet = new CSSStyleSheet();
                shadowSheet.replaceSync("span { color: rgb(255, 0, 0) }");
                root.adoptedStyleSheets = [shadowSheet];
                const color = (element) => getComputedStyle(element).color;
                const before = [color(document.getElementById("p")), color(document.getElementById("q")), color(root.getElementById("inside"))];
                shadowSheet.replaceSync("span { color: rgb(1, 2, 3) }");
                sheet.disabled = true;
                before.push(color(root.getElementById("inside")), color(document.getElementById("p")));
                document.getElementById("status").textContent = before.join("|");
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some("rgb(0, 128, 0)|rgb(0, 0, 255)|rgb(255, 0, 0)|rgb(1, 2, 3)|rgb(0, 0, 0)")
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
    fn links_out_of_the_app_go_to_the_host_and_the_page_stays() {
        struct Recorded(RefCell<Vec<String>>);
        impl LinkOpener for Recorded {
            fn open(&self, url: &str) {
                self.0.borrow_mut().push(url.to_string());
            }
        }
        let page = r##"<a id="out" href="https://example.com/page">out</a><a id="mail" href="mailto:hi@example.com">mail</a><a id="void" href="javascript:void(0)">void</a><p id="status">idle</p><script>
            document.getElementById("status").textContent = [window.open("https://example.com/new"), window.open("/inner"), window.open()].join(",");
        </script>"##;
        let opened = Rc::new(Recorded(RefCell::new(Vec::new())));
        let mut browser = Browser::new(bundle(page)).with_link_opener(opened.clone());
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        browser
            .click_id("out")
            .expect("an outside link should not fail");
        browser
            .click_id("mail")
            .expect("a mail link should not fail");
        browser
            .click_id("void")
            .expect("a javascript: link should not fail");
        browser
            .page
            .as_mut()
            .expect("page")
            .runtime
            .execute(r#"location.href = "https://example.org/";"#)
            .expect("script should run");
        browser
            .process_pending_navigation()
            .expect("an outside location should not fail");
        assert_eq!(
            *opened.0.borrow(),
            [
                "https://example.com/new",
                "https://example.com/page",
                "mailto:hi@example.com",
                "https://example.org/"
            ]
        );
        assert_eq!(
            browser
                .current_url
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some("https://app.ceri.invalid/"),
            "the page stays, and window.open of an app page opens nothing"
        );
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status");
        assert_eq!(document.borrow().text_content(status), ",,");

        let mut bare = Browser::new(bundle(page));
        bare.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        assert!(
            bare.click_id("out").is_err(),
            "without an opener it fails as before"
        );
    }

    #[test]
    fn popovers_show_hide_and_dismiss_as_chrome_does() {
        let mut browser = Browser::new(bundle(
            r##"<body style="margin: 0"><button id="opener" popovertarget="a" style="position: fixed; left: 0; top: 0; width: 40px; height: 20px">a</button><div id="a" popover style="width: 100px; height: 40px">A<div id="inner" popover>inner</div></div><div id="b" popover>B</div><div id="m" popover="manual">M</div><p id="status"></p><script>
                const $ = (id) => document.getElementById(id);
                const out = [];
                const log = [];
                for (const id of ["a", "b", "inner"]) {
                    $(id).addEventListener("toggle", (event) => log.push(id + ":" + event.oldState + ">" + event.newState));
                }
                $("a").showPopover();
                $("m").showPopover();
                $("inner").showPopover();
                out.push($("a").matches(":popover-open"), $("inner").matches(":popover-open"), document.querySelectorAll(":popover-open").length);
                $("b").showPopover();
                out.push($("a").matches(":popover-open"), $("inner").matches(":popover-open"), $("m").matches(":popover-open"));
                out.push(getComputedStyle($("b")).position, getComputedStyle($("a")).display);
                $("b").addEventListener("beforetoggle", (event) => { if (event.newState === "open") event.preventDefault(); });
                $("b").hidePopover();
                $("b").showPopover();
                out.push($("b").matches(":popover-open"));
                $("opener").click();
                out.push($("a").matches(":popover-open"));
                $("status").textContent = out.join(" ");
                setTimeout(() => { $("status").textContent += " | " + log.join(" "); }, 0);
            </script></body>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        browser.advance_time(10).expect("tasks should run");
        let status = |browser: &mut Browser| {
            let document = browser.document().expect("document should exist");
            let document = document.borrow();
            let status = document.find_element_by_id("status").expect("status");
            document.text_content(status)
        };
        assert_eq!(
            status(&mut browser),
            "true true 3 false false true fixed none false true | inner:closed>closed b:closed>closed a:closed>open"
        );
        let open = |browser: &mut Browser, id: &str| {
            let document = browser.document().expect("document should exist");
            let node = document.borrow().find_element_by_id(id).expect("popover");
            browser
                .page
                .as_ref()
                .expect("page")
                .runtime
                .selector_state(None)
                .popovers
                .contains(&node)
        };
        let viewport = Size {
            width: 400.0,
            height: 300.0,
        };
        // a press on the button that opens it keeps it open; a press elsewhere closes it
        browser
            .dispatch_at(viewport, 10.0, 10.0, "pointerdown")
            .expect("press should dispatch");
        assert!(open(&mut browser, "a"), "the invoker counts as inside");
        browser
            .dispatch_at(viewport, 390.0, 290.0, "pointerdown")
            .expect("press should dispatch");
        assert!(!open(&mut browser, "a"), "a press outside dismisses");
        assert!(open(&mut browser, "m"), "a manual popover stays");
        browser
            .page
            .as_mut()
            .expect("page")
            .runtime
            .execute(r#"document.getElementById("a").showPopover();"#)
            .expect("script should run");
        browser.key_down("Escape").expect("escape should dispatch");
        assert!(
            !open(&mut browser, "a"),
            "escape closes the top auto popover"
        );
        assert!(open(&mut browser, "m"));
    }

    #[test]
    fn ranges_measure_the_text_and_elements_they_select() {
        let mut browser = Browser::new(bundle(
            r##"<body style="margin: 8px; user-select: none"><p id="p">Hello <b id="b">bold world</b> and more</p><div id="w" style="width: 60px">one two three four</div><p id="status"></p><script>
                const t = document.getElementById("p").firstChild, b = document.getElementById("b");
                const word = document.getElementById("b").getBoundingClientRect();
                const out = [];
                let r = document.createRange(); r.setStart(t, 1); r.setEnd(t, 4);
                const part = r.getBoundingClientRect();
                out.push(r.getClientRects().length, part.width > 0, part.height > 0);
                r = document.createRange(); r.setStart(t, 2); r.setEnd(b.nextSibling, 3);
                const rects = [...r.getClientRects()];
                out.push(rects.length, rects[1].left === word.left && rects[1].width === word.width, rects[2].width === rects[1].width);
                const bound = r.getBoundingClientRect();
                out.push(bound.left === rects[0].left && bound.right === rects[3].right);
                r = document.createRange(); r.setStart(t, 3); r.setEnd(t, 3);
                out.push(r.getClientRects().length, r.getBoundingClientRect().width);
                r = document.createRange(); r.selectNodeContents(document.getElementById("w").firstChild);
                const lines = [...r.getClientRects()];
                out.push(lines.length > 1, lines.every((line, index) => index === 0 || line.top > lines[index - 1].top));
                document.getElementById("status").textContent = out.join(" ");
            </script></body>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.text_content(status),
            "1 true true 4 true true true 1 0 true true"
        );
    }

    #[test]
    fn themed_checkboxes_radios_and_sliders_sit_on_their_border_edge() {
        let mut browser = Browser::new(bundle(
            r##"<body style="margin: 0"><div>Xy<span id="plain" style="display: inline-block; width: 13px; height: 13px"></span><input id="radio" type="radio" style="margin: 0 0 10px 0"><input id="box" type="checkbox" style="margin: 0 0 10px 0"><input id="range" type="range" style="margin: 0 0 10px 0"><input id="flat" type="checkbox" style="margin: 0 0 10px 0; appearance: none; width: 13px; height: 13px"></div><p id="status"></p><script>
                const bottom = (id) => document.getElementById(id).getBoundingClientRect().bottom;
                const plain = bottom("plain");
                document.getElementById("status").textContent = ["radio", "box", "range", "flat"]
                    .map((id) => bottom(id) - plain).join(" ");
            </script></body>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(document.text_content(status), "0 0 0 -10");
    }

    #[test]
    fn text_at_a_font_size_of_zero_takes_no_room_and_draws_nothing() {
        let mut browser = Browser::new(bundle(
            r##"<body style="margin: 0; background: #000; color: #fff; font: 20px/1.5 sans-serif"><div id="gone" style="font-size: 0">hidden <span id="em" style="display: inline-block; width: 2em; height: 2em"></span></div><div id="back" style="font-size: 0"><b id="shown" style="font-size: 20px">shown</b></div><p id="status"></p><script>
                const box = (id) => document.getElementById(id).getBoundingClientRect();
                const style = (id) => getComputedStyle(document.getElementById(id));
                document.getElementById("status").textContent = [
                    style("gone").fontSize, box("gone").height, box("em").width,
                    style("shown").fontSize, box("back").height > 0,
                ].join(" ");
            </script></body>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let canvas = browser
            .paint(Size {
                width: 200.0,
                height: 100.0,
            })
            .expect("page should paint");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(document.text_content(status), "0px 0 0 20px true");
        drop(canvas);
    }

    #[test]
    fn forced_colours_replace_what_the_author_chose_unless_it_is_a_system_colour() {
        let page = r##"<style>
            body { color: #123456; background: #abcdef }
            #plain { color: red; background: blue; border: 2px solid green; box-shadow: 0 0 4px red }
            #clear { background: transparent }
            #kept { forced-color-adjust: none; color: red }
            #named { color: GrayText; border: 1px solid Highlight }
            #flag { width: 1px }
            @media (forced-colors: active) { #flag { width: 7px } }
            @media (prefers-contrast: more) { #flag { height: 9px } }
            </style>
            <p id="plain">a <b id="inner">b</b></p><p id="clear">c</p><p id="kept">d <b id="under">e</b></p>
            <p id="named">f</p><a id="link" href="/x" style="color: purple">g</a>
            <button id="press" style="color: red; background: pink">h</button><div id="flag"></div>
            <p id="status"></p><script>
            window.read = () => {
                const style = (id) => getComputedStyle(document.getElementById(id));
                const flag = document.getElementById("flag").getBoundingClientRect();
                document.getElementById("status").textContent = [
                    style("plain").color, style("plain").backgroundColor, style("plain").borderTopColor,
                    style("plain").boxShadow, style("inner").color, style("clear").backgroundColor,
                    style("kept").color, style("under").color, style("named").color,
                    style("named").borderTopColor, style("link").color, style("press").color,
                    style("press").backgroundColor, flag.width + "x" + flag.height,
                    matchMedia("(forced-colors: active)").matches,
                ].join("|");
            };
            read();
            setInterval(read, 10);
            </script>"##;
        let mut browser = Browser::new(bundle(page));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let status = |browser: &mut Browser| {
            browser.advance_time(20).expect("the page reads its styles");
            let document = browser.document().expect("document should exist");
            let document = document.borrow();
            let status = document.find_element_by_id("status").expect("status");
            document.text_content(status)
        };
        let (red, blue, green) = ("rgb(255, 0, 0)", "rgb(0, 0, 255)", "rgb(0, 128, 0)");
        assert_eq!(
            status(&mut browser),
            format!(
                "{red}|{blue}|{green}|rgb(255, 0, 0) 0px 0px 4px 0px|{red}|rgba(0, 0, 0, 0)|{red}|{red}|rgb(128, 128, 128)|rgb(0, 120, 215)|rgb(128, 0, 128)|{red}|rgb(255, 192, 203)|1x0|false"
            )
        );
        browser.set_user_preferences(UserPreferences {
            forced_colors: Some(crate::css::SystemColors::high_contrast()),
            ..UserPreferences::default()
        });
        let (white, black) = ("rgb(255, 255, 255)", "rgb(0, 0, 0)");
        // the palette's grey and highlight stand where the author named them, a link and a
        // button take their own pair, and what forced-color-adjust: none covers is left alone
        assert_eq!(
            status(&mut browser),
            format!(
                "{white}|{black}|{white}|none|{white}|rgba(0, 0, 0, 0)|{red}|{red}|rgb(63, 242, 63)|rgb(26, 235, 255)|rgb(255, 255, 0)|{white}|{black}|7x9|true"
            )
        );
    }

    #[test]
    fn a_canvas_measures_at_its_own_size_before_the_page_first_lays_out() {
        let mut browser = Browser::new(bundle(
            r#"<body style="margin: 0"><canvas id="c" width="300" height="120" style="display: block"></canvas><canvas id="wide" width="300" height="120" style="display: block; width: 100px"></canvas><p id="status"></p><script>
                const size = (id) => {
                    const canvas = document.getElementById(id);
                    const box = canvas.getBoundingClientRect();
                    return [box.width, box.height, canvas.clientWidth, canvas.clientHeight].join("x");
                };
                document.getElementById("status").textContent = size("c") + " " + size("wide");
            </script></body>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.text_content(status),
            "300x120x300x120 100x40x100x40"
        );
    }

    #[test]
    fn a_control_takes_its_own_font_and_line_height_where_the_page_sets_others() {
        let mut browser = Browser::new(bundle(
            r#"<style>body { font: italic bold 20px/1.45 Georgia, serif }</style><button id="press">B</button><input id="field"><p id="status">idle</p><script>
                const read = (id) => {
                    const style = getComputedStyle(document.getElementById(id));
                    return [style.fontStyle, style.fontWeight, style.fontSize, style.lineHeight, style.fontFamily].join(" ");
                };
                document.getElementById("status").textContent = read("press") + "|" + read("field");
            </script>"#,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        let control = "normal 400 13.3333px normal Arial";
        assert_eq!(
            document.element_text_content(status),
            Some(format!("{control}|{control}"))
        );
    }

    #[test]
    fn computed_fonts_serialize_and_element_from_point_hits_as_chrome_does() {
        let mut browser = Browser::new(bundle(
            r#"<style>body { margin: 0; font: 13px/1.4 "Lucida Grande", Lucida Sans Unicode, sans-serif } #head { height: 20px; font-size: 10px; letter-spacing: .06em } button { font: inherit } .same { font-family: inherit }</style><div id="head">A</div><button id="press">B</button><span class="same" id="note">C</span><p id="status">idle</p><script>
                const style = (id) => getComputedStyle(document.getElementById(id));
                document.getElementById("status").textContent = [
                    style("head").fontFamily, style("head").letterSpacing, style("press").fontFamily,
                    style("note").fontFamily, style("note").lineHeight,
                    document.elementFromPoint(2, 2).id, document.elementFromPoint(-1, 2),
                    document.elementFromPoint(390, 290).tagName,
                ].join("|");
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
            .expect("font page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        let family = r#""Lucida Grande", "Lucida Sans Unicode", sans-serif"#;
        assert_eq!(
            document.element_text_content(status),
            Some(format!(
                "{family}|0.6px|{family}|{family}|18.2px|head||HTML"
            ))
        );
    }

    #[test]
    fn visibility_hit_lists_and_scrolling_into_view_follow_the_layout() {
        let mut browser = Browser::new(bundle(
            r#"<style>body { margin: 0; font: 16px/20px sans-serif } div { margin: 0 }</style><div id="outer" style="height: 50px; overflow: auto"><div id="spacer" style="height: 200px"></div><div id="target" style="height: 20px">t</div></div><div id="hidden" style="display: none">h</div><div id="faded" style="opacity: 0"><span id="inner">x</span></div><div id="invisible" style="visibility: hidden">i</div><div style="height: 1000px"></div><div id="far">far</div><p id="status">idle</p><script>
                const $ = (id) => document.getElementById(id);
                const hits = document.elementsFromPoint(2, 2).map((element) => element.id || element.tagName).join(" ");
                const visible = [
                    $("hidden").checkVisibility(), $("inner").checkVisibility(),
                    $("inner").checkVisibility({ checkOpacity: true }), $("invisible").checkVisibility(),
                    $("invisible").checkVisibility({ visibilityProperty: true }),
                ].join(" ");
                $("target").scrollIntoView();
                const first = [$("outer").scrollTop, scrollY].join(" ");
                $("far").scrollIntoView({ block: "end" });
                $("status").textContent = [hits, visible, first, Math.round($("far").getBoundingClientRect().bottom)].join("|");
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
            .expect("page should load");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some("spacer outer BODY HTML|false true false true false|170 30|300")
        );
    }

    #[test]
    fn styles_script_adds_rewrites_and_removes_apply_as_they_change() {
        let mut resources = bundle(
            r##"<div id="box" style="height: 10px"></div><p id="status">idle</p><script>
                const box = document.getElementById("box");
                const read = () => getComputedStyle(box).color + "/" + getComputedStyle(box).marginLeft;
                const log = [read()];
                const style = document.createElement("style");
                style.textContent = "#box { color: rgb(1, 2, 3) }";
                document.head.appendChild(style);
                log.push(read());
                const link = document.createElement("link");
                link.rel = "stylesheet";
                link.href = "/late.css";
                document.head.appendChild(link);
                log.push(read());
                style.textContent = "#box { color: rgb(4, 5, 6) }";
                log.push(read());
                link.remove();
                log.push(read());
                document.getElementById("status").textContent = log.join(",");
                setTimeout(() => {
                    const wide = document.createElement("style");
                    wide.textContent = "#box { width: 37px }";
                    document.head.append(wide);
                }, 5);
            </script>"##,
        );
        resources
            .insert(
                "/late.css",
                "text/css",
                b"#box { margin-left: 3px }".to_vec(),
            )
            .expect("stylesheet should be accepted");
        let mut browser = Browser::new(resources);
        let viewport = Size {
            width: 400.0,
            height: 300.0,
        };
        browser.set_viewport(viewport, 1.0);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let document = browser.document().expect("document should exist");
        let (status, target) = {
            let document = document.borrow();
            let status = document.find_element_by_id("status").expect("status");
            (
                document.element_text_content(status),
                document.find_element_by_id("box").expect("box"),
            )
        };
        assert_eq!(
            status.as_deref(),
            Some(
                "rgb(0, 0, 0)/0px,rgb(1, 2, 3)/0px,rgb(1, 2, 3)/3px,rgb(4, 5, 6)/3px,rgb(4, 5, 6)/0px"
            )
        );
        let hit = |browser: &Browser, x| browser.layout(viewport).expect("layout").hit_test(x, 5.0);
        assert_eq!(hit(&browser, 300.0), Some(target));
        browser.advance_time(10).expect("timer should run");
        assert_eq!(hit(&browser, 30.0), Some(target));
        assert_ne!(hit(&browser, 300.0), Some(target));
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
    fn failing_scripts_are_reported_and_the_page_goes_on() {
        let mut browser = Browser::new(bundle(
            r#"<p id="status">idle</p><script>missing?</script>
            <script>throw new RangeError("late");</script>
            <script src="/gone.js"></script>
            <script>document.getElementById("status").textContent = "ran";</script>"#,
        ));
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("the page should load"),
            NavigationStatus::Loaded
        );
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("ran")
        );
        let logs = browser.console_logs().join("\n");
        assert!(logs.contains("Uncaught SyntaxError"), "{logs}");
        assert!(logs.contains("Uncaught RangeError: late"), "{logs}");
        assert!(logs.contains("script failed to load"), "{logs}");
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
    fn a_frame_changed_in_two_places_is_one_revision_of_two_boxes() {
        let mut browser = Browser::new(bundle(
            r##"<style>body { margin: 0 }
            div { position: absolute; width: 10px; height: 10px; background: red }</style>
            <div id="near" style="left: 0; top: 0"></div>
            <div id="far" style="left: 200px; top: 150px"></div>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let viewport = Size {
            width: 320.0,
            height: 180.0,
        };
        let shown = browser.paint(viewport).unwrap().revision().id;
        let document = browser.document().unwrap();
        for id in ["near", "far"] {
            let node = document.borrow().find_element_by_id(id).unwrap();
            document
                .borrow_mut()
                .set_style_property(node, "background", "blue".to_string());
        }
        let (base, boxes) = browser
            .paint(viewport)
            .unwrap()
            .revision()
            .changed
            .expect("the frame should repaint over the last");
        assert_eq!(base, shown);
        assert_eq!(
            boxes.iter().collect::<Vec<_>>(),
            [[0, 0, 10, 10], [200, 150, 210, 160]]
        );
    }

    #[test]
    fn a_see_through_frame_is_clear_where_the_page_paints_nothing() {
        let mut browser = Browser::new(bundle(
            r##"<style>html, body { margin: 0; background: transparent; }
            div { position: absolute; left: 20px; top: 20px; width: 40px; height: 40px; background: red; }</style>
            <div id="box"></div>"##,
        ));
        browser.set_see_through(true);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let viewport = Size {
            width: 120.0,
            height: 90.0,
        };
        let first = browser.paint(viewport).unwrap();
        assert_eq!(pixel_at(&first, 5, 5), [0, 0, 0, 0]);
        assert_eq!(pixel_at(&first, 30, 30), [255, 0, 0, 255]);
        // the box moves, and what it leaves behind is clear again
        let document = browser.document().unwrap();
        let node = document.borrow().find_element_by_id("box").unwrap();
        document
            .borrow_mut()
            .set_style_property(node, "left", "70px".to_string());
        let second = browser.paint(viewport).unwrap();
        assert!(second.revision().changed.is_some(), "painted over the last");
        assert_eq!(pixel_at(&second, 30, 30), [0, 0, 0, 0]);
        assert_eq!(pixel_at(&second, 80, 30), [255, 0, 0, 255]);
    }

    // a few hundred seeded writes, pointer moves and scrolls, each followed by a frame painted
    // over an earlier one while the last is still held; it must not differ from the same
    // layout painted whole
    #[test]
    fn a_frame_painted_over_an_earlier_one_matches_a_whole_paint_after_any_change() {
        let mut browser = Browser::new(bundle(
            r##"<style>
            body { margin: 0; font-size: 14px }
            #root { position: relative; width: 360px }
            .card { margin: 6px; padding: 6px; border: 1px solid #456; border-radius: 6px;
                box-shadow: 0 3px 8px #0006; background: linear-gradient(#eef, #cdf) }
            .ring { outline: 3px solid #c33; outline-offset: 2px }
            .glow { box-shadow: 0 0 12px 4px #39f, inset 0 0 6px #000 }
            .dark { background: #223; color: #fff; text-shadow: 1px 2px 3px #f00 }
            .abs { position: absolute; left: 200px; top: 20px; width: 40px; height: 40px;
                background: #f80 }
            .tilt { transform: rotate(8deg) }
            .fade { opacity: 0.5 }
            #scroller { height: 60px; overflow: auto }
            span:hover, .card:hover { background: #ffc }
            li { margin-left: 20px }
            </style>
            <div id="root"><div class="card"><span>one</span> two <b>three</b></div><div class="card glow"><input type="checkbox" checked><input type="range" value="30"><input value="text"></div><ul><li>first</li><li class="ring">second</li></ul><div id="scroller"><p>a</p><p>b</p><p>c</p><p>d</p><p>e</p><p>f</p></div><div class="abs"></div><p>tail text that wraps across the width of the root box more than once to make lines</p></div>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let viewport = Size {
            width: 400.0,
            height: 320.0,
        };
        let document = browser.document().unwrap();
        let root = document.borrow().find_element_by_id("root").unwrap();
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut pick = |below: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as usize % below
        };
        const PROPERTIES: [(&str, [&str; 4]); 12] = [
            ("left", ["3px", "120px", "auto", "40%"]),
            ("top", ["0", "33px", "auto", "70px"]),
            ("width", ["30px", "55%", "auto", "140px"]),
            ("height", ["18px", "44px", "auto", "9px"]),
            ("margin-left", ["0", "9px", "30px", "-4px"]),
            (
                "background",
                ["#e33", "#3a6", "none", "linear-gradient(90deg, #fff, #339)"],
            ),
            ("color", ["#000", "#c0c", "#fff", "#06c"]),
            (
                "box-shadow",
                [
                    "none",
                    "0 0 9px #000",
                    "4px 4px 0 #f00",
                    "inset 0 2px 5px #00f",
                ],
            ),
            (
                "outline",
                [
                    "none",
                    "2px solid #0a0",
                    "5px solid #00f",
                    "1px dotted #000",
                ],
            ),
            ("opacity", ["1", "0.4", "0.8", "1"]),
            (
                "transform",
                ["none", "translate(7px, 3px)", "rotate(5deg)", "none"],
            ),
            ("border-radius", ["0", "4px", "50%", "12px 2px"]),
        ];
        const CLASSES: [&str; 8] = [
            "card",
            "card glow",
            "ring",
            "dark",
            "abs",
            "tilt",
            "fade",
            "",
        ];
        const TEXTS: [&str; 4] = [
            "a",
            "longer words here to wrap around the line",
            "",
            "x y z",
        ];
        let mut held = browser.paint(viewport).unwrap();
        for step in 0..200 {
            let (elements, texts) = {
                let document = document.borrow();
                let nodes = document.subtree(root);
                let texts = nodes
                    .iter()
                    .copied()
                    .filter(|&node| {
                        matches!(document.kind(node), Some(crate::dom::NodeKind::Text(_)))
                    })
                    .collect::<Vec<_>>();
                let elements = nodes
                    .into_iter()
                    .filter(|&node| document.is_element(node))
                    .collect::<Vec<_>>();
                (elements, texts)
            };
            let element = elements[pick(elements.len())];
            let change = pick(9);
            let mut scrolled = false;
            match change {
                0..=2 => {
                    let (property, values) = PROPERTIES[pick(PROPERTIES.len())];
                    document.borrow_mut().set_style_property(
                        element,
                        property,
                        values[pick(4)].to_string(),
                    );
                }
                3 => {
                    document.borrow_mut().set_attribute(
                        element,
                        "class",
                        CLASSES[pick(8)].to_string(),
                    );
                }
                4 if !texts.is_empty() => {
                    document
                        .borrow_mut()
                        .set_character_data(texts[pick(texts.len())], TEXTS[pick(4)].to_string());
                }
                5 => {
                    let mut document = document.borrow_mut();
                    let added = document.create_element(["div", "span", "p"][pick(3)]);
                    let words = document.create_text(TEXTS[pick(4)]);
                    document
                        .append_child(added, words)
                        .expect("text should append");
                    document
                        .insert_before(element, added, None)
                        .expect("element should insert");
                }
                6 if element != root && elements.len() > 8 => {
                    document.borrow_mut().remove(element);
                }
                7 => {
                    browser
                        .dispatch_at(viewport, pick(380) as f32, pick(300) as f32, "pointermove")
                        .expect("pointer move should dispatch");
                }
                _ => {
                    scrolled = true;
                    browser
                        .scroll_at(
                            viewport,
                            50.0,
                            pick(300) as f32,
                            0.0,
                            pick(60) as f32 - 30.0,
                        )
                        .expect("scroll should dispatch");
                }
            }
            let frame = browser.paint(viewport).unwrap();
            let layout = browser.layout(viewport).unwrap();
            let whole = crate::paint::paint_with_canvases(&layout, &[], 1.0, false);
            // pixels a scroll moves were drawn at another place, where a gradient or a blur
            // can round a step differently, so a scrolled frame may be off by one and the
            // next frame starts from a whole paint
            let furthest = frame
                .rgba_pixels()
                .iter()
                .zip(whole.rgba_pixels())
                .map(|(painted, wanted)| painted.abs_diff(*wanted))
                .max();
            assert!(
                furthest <= Some(u8::from(scrolled)),
                "after change {change} of step {step}: off by {furthest:?}"
            );
            if scrolled {
                *browser.painted.borrow_mut() = None;
            }
            // the frame before stays held through the next paint, as a window holds it
            held = frame;
        }
        drop(held);
    }

    #[test]
    fn a_frame_still_held_is_left_alone_and_the_one_before_it_is_painted_over() {
        let viewport = Size {
            width: 320.0,
            height: 180.0,
        };
        let page = |left: &str| {
            let mut browser = Browser::new(bundle(&format!(
                r##"<style>body {{ margin: 0 }}
                div {{ position: absolute; top: 0; width: 10px; height: 10px; background: red }}
                </style><div id="box" style="left: {left}"></div>"##
            )));
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("page should load");
            browser
        };
        let browser = page("0");
        let document = browser.document().unwrap();
        let node = document.borrow().find_element_by_id("box").unwrap();
        let move_to = |left: &str| {
            document
                .borrow_mut()
                .set_style_property(node, "left", left.to_string());
        };
        let first = browser.paint(viewport).unwrap();
        move_to("40px");
        let second = browser.paint(viewport).unwrap();
        let buffer = first.rgba_pixels().as_ptr();
        drop(first);
        move_to("80px");
        let third = browser.paint(viewport).unwrap();
        assert_eq!(third.rgba_pixels().as_ptr(), buffer);
        assert_eq!(
            third.revision().changed.map(|(base, _)| base),
            Some(second.revision().id)
        );
        for (frame, left) in [(&second, "40px"), (&third, "80px")] {
            let whole = page(left).paint(viewport).unwrap();
            assert!(frame.rgba_pixels() == whole.rgba_pixels(), "at {left}");
        }
    }

    #[test]
    fn setting_an_attribute_or_a_class_to_what_it_is_is_observed_and_restyles_nothing() {
        let mut document = crate::dom::Document::parse_markup(
            r#"<main><button id="go" class="gel" disabled style="left: 4px">go</button></main>"#,
        )
        .expect("document should parse");
        let button = document.find_element_by_id("go").unwrap();
        document.observe_mutations(true);
        let before = (
            document.version(),
            document.selector_version(),
            document.change_stamp(),
        );
        document.set_attribute(button, "disabled", String::new());
        document.set_attribute(button, "class", "gel".to_string());
        document.set_style_property(button, "left", "4px".to_string());
        document.remove_attribute(button, "hidden");
        assert_eq!(document.take_mutations().len(), 3);
        assert_eq!(
            (document.version(), document.selector_version()),
            (before.0, before.1)
        );
        assert!(document.unwritten_since(button, before.2));
        document.set_attribute(button, "class", "gel wide".to_string());
        assert_ne!(
            (document.version(), document.selector_version()),
            (before.0, before.1)
        );
        assert!(!document.unwritten_since(button, before.2));
    }

    #[test]
    fn the_body_is_active_with_nothing_focused_and_image_data_can_be_made_blank() {
        let mut browser = Browser::new(bundle(
            r##"<input id="field"><canvas id="pad" width="4" height="4"></canvas><p id="status"></p>
            <script>
                const log = [document.activeElement === document.body];
                const field = document.getElementById("field");
                field.focus();
                log.push(document.activeElement === field);
                field.blur();
                log.push(document.activeElement === document.body);
                log.push(CSS.supports("touch-action", "none"), CSS.supports("forced-color-adjust", "none"));
                const pen = document.getElementById("pad").getContext("2d");
                const blank = pen.createImageData(3, -2);
                const same = pen.createImageData(blank);
                log.push(blank.width, blank.height, blank.data.length, same.data.length);
                log.push(blank.data.every((byte) => byte === 0));
                const made = new ImageData(2, 3);
                const wrapped = new ImageData(new Uint8ClampedArray(16), 2);
                log.push(made.data.length, wrapped.width + "x" + wrapped.height);
                try {
                    new ImageData(new Uint8ClampedArray(10), 2);
                } catch (error) {
                    log.push(error.name);
                }
                try {
                    pen.createImageData(0, 2);
                } catch (error) {
                    log.push(error.name);
                }
                document.getElementById("status").textContent = log.join(" ");
            </script>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let document = browser.document().unwrap();
        let document = document.borrow();
        let status = document.find_element_by_id("status").unwrap();
        assert_eq!(
            document.text_content(status),
            "true true true true true 3 2 24 24 true 24 2x2 IndexSizeError IndexSizeError"
        );
    }

    #[test]
    fn a_hidden_page_hears_of_it_and_runs_no_animation_frames_until_shown() {
        let mut browser = Browser::new(bundle(
            r##"<p id="count">0</p><p id="seen"></p><script>
                let count = 0;
                const frame = () => {
                    document.getElementById("count").textContent = ++count;
                    requestAnimationFrame(frame);
                };
                requestAnimationFrame(frame);
                let ticks = 0;
                setInterval(() => ticks++, 10);
                document.addEventListener("visibilitychange", () => {
                    document.getElementById("seen").textContent +=
                        document.visibilityState + ":" + document.hidden + ":" + ticks + " ";
                });
            </script>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let text = |browser: &Browser, id: &str| {
            let document = browser.document().unwrap();
            let document = document.borrow();
            document.text_content(document.find_element_by_id(id).unwrap())
        };
        let run = |browser: &mut Browser, milliseconds: u32| {
            for _ in 0..milliseconds {
                browser.advance_time(1).expect("time should advance");
            }
        };
        run(&mut browser, 100);
        assert_eq!(text(&browser, "count"), "6");
        browser.set_visible(false).expect("the page should hide");
        assert_eq!(browser.next_wake(), Some(10));
        run(&mut browser, 500);
        // timers went on while no frame ran
        assert_eq!(text(&browser, "count"), "6");
        browser.set_visible(true).expect("the page should show");
        run(&mut browser, 100);
        assert_eq!(text(&browser, "count"), "13");
        assert_eq!(text(&browser, "seen"), "hidden:true:10 visible:false:60 ");
    }

    #[test]
    fn positioned_boxes_and_level_zero_contexts_stack_in_tree_order_over_content_in_flow() {
        let pixel_and_hit = |body: &str| {
            let mut browser = Browser::new(bundle(&format!(
                "<style>body {{ margin: 0 }} div, span {{ display: block }}</style>{body}"
            )));
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("page should load");
            let viewport = Size {
                width: 120.0,
                height: 120.0,
            };
            let canvas = browser.paint(viewport).unwrap();
            let at = (90 * canvas.width() as usize + 50) * 4;
            let pixel = canvas.rgba_pixels()[at..at + 3].to_vec();
            let hit = browser.layout(viewport).unwrap().hit_test(50.0, 90.0);
            let document = browser.document().unwrap();
            let id =
                hit.and_then(|node| document.borrow().attribute(node, "id").map(str::to_string));
            (pixel, id)
        };
        let blue = (vec![0, 0, 255], Some("over".to_string()));
        let over = "<span id='over' style='position:absolute;inset:auto 0 0;height:40px;background:blue'></span>";
        // a filtered box, alone or inside a positioned one, lies under a later positioned box
        for under in [
            "<div style='position:absolute;inset:0;background:red;filter:brightness(0.5)'></div>",
            "<div style='position:absolute;inset:0;overflow:hidden'><div style='position:absolute;inset:0;background:red;filter:brightness(0.5)'></div></div>",
            "<div style='width:100px;height:100px;background:red;transform:translateX(1px)'></div>",
        ] {
            let page = format!(
                "<div style='position:relative;width:100px;height:100px'>{under}{over}</div>"
            );
            assert_eq!(pixel_and_hit(&page), blue, "{under}");
        }
        // a positioned box lies over content in flow that comes after it
        let page = format!(
            "<div style='position:relative;width:100px;height:100px'>{over}<div style='height:100px;background:red'></div></div>"
        );
        assert_eq!(pixel_and_hit(&page), blue);
    }

    #[test]
    fn a_non_scaling_stroke_keeps_its_width_in_a_stretched_svg() {
        let stroke_height = |effect: &str| {
            let mut browser = Browser::new(bundle(&format!(
                r##"<style>body {{ margin: 0 }} svg {{ display: block; width: 100px; height: 40px }}
                path {{ stroke: #f00; stroke-width: 1; fill: none; vector-effect: {effect} }}</style>
                <svg viewBox="0 0 100 2" preserveAspectRatio="none"><path d="M0 1H100" /></svg>"##
            )));
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("page should load");
            let canvas = browser
                .paint(Size {
                    width: 120.0,
                    height: 60.0,
                })
                .unwrap();
            // red over white leaves the green channel short by the row's coverage
            let covered = (0..40)
                .map(|row| {
                    let at = (row * canvas.width() as usize + 50) * 4;
                    f32::from(255 - canvas.rgba_pixels()[at + 1]) / 255.0
                })
                .sum::<f32>();
            (covered * 10.0).round() / 10.0
        };
        // the viewBox stretches twenty times down: a scaling stroke grows with it
        assert_eq!(stroke_height("none"), 20.0);
        assert_eq!(stroke_height("non-scaling-stroke"), 1.0);
    }

    #[test]
    fn computed_width_and_height_are_of_the_box_that_box_sizing_names() {
        let mut browser = Browser::new(bundle(
            r##"<style>div { width: 100px; height: 40px; padding: 10px; border: 2px solid }
            #border { box-sizing: border-box }</style>
            <div id="content"></div><div id="border"></div><p id="status"></p><script>
                const read = (id) => {
                    const style = getComputedStyle(document.getElementById(id));
                    return style.width + " " + style.height;
                };
                document.getElementById("status").textContent = read("content") + ", " + read("border");
            </script>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let document = browser.document().unwrap();
        let document = document.borrow();
        let status = document.find_element_by_id("status").unwrap();
        assert_eq!(document.text_content(status), "100px 40px, 100px 40px");
    }

    #[test]
    fn computed_border_widths_snap_to_device_pixels_with_or_without_a_box() {
        let mut browser = Browser::new(bundle(
            r##"<style>div { border: 1px solid; border-left-width: 2.5px }</style>
            <div id="shown"></div><div id="hidden" style="display: none"></div>
            <p id="status"></p><script>
                const read = (id) => {
                    const style = getComputedStyle(document.getElementById(id));
                    return style.borderTopWidth + " " + style.borderLeftWidth;
                };
                document.getElementById("status").textContent = read("shown") + ", " + read("hidden");
            </script>"##,
        ));
        browser.set_viewport(
            Size {
                width: 300.0,
                height: 200.0,
            },
            1.5,
        );
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let document = browser.document().unwrap();
        let document = document.borrow();
        let status = document.find_element_by_id("status").unwrap();
        // one and a half device pixels round down to one, three and three quarters to three
        assert_eq!(
            document.text_content(status),
            "0.666667px 2px, 0.666667px 2px"
        );
    }

    #[test]
    fn computed_display_is_block_level_for_items_floats_and_out_of_flow_boxes() {
        let mut browser = Browser::new(bundle(
            r##"<span id="plain">a</span>
            <div style="display: flex"><span id="item">b</span></div>
            <div style="display: grid"><span id="cell" style="display: inline-flex">c</span></div>
            <span id="float" style="float: left">d</span>
            <span id="absolute" style="position: absolute; display: inline-block">e</span>
            <p id="status"></p><script>
                document.getElementById("status").textContent =
                    ["plain", "item", "cell", "float", "absolute"]
                        .map((id) => getComputedStyle(document.getElementById(id)).display)
                        .join(" ") + " " + getComputedStyle(document.documentElement).display;
            </script>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let document = browser.document().unwrap();
        let document = document.borrow();
        let status = document.find_element_by_id("status").unwrap();
        assert_eq!(
            document.text_content(status),
            "inline block flex block block block"
        );
    }

    #[test]
    fn animation_frames_come_as_often_as_the_display_refreshes() {
        assert_eq!(
            [30_000, 60_000, 120_000, 144_000, 240_000, 500_000].map(frame_interval),
            [16, 16, 8, 6, 4, 4]
        );
        let frames_in_a_second = |rate: Option<u32>| {
            let mut browser = Browser::new(bundle(
                r##"<p id="count">0</p><script>
                    let count = 0;
                    const frame = () => {
                        document.getElementById("count").textContent = ++count;
                        requestAnimationFrame(frame);
                    };
                    requestAnimationFrame(frame);
                </script>"##,
            ));
            if let Some(rate) = rate {
                browser.set_refresh_rate(rate);
            }
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("page should load");
            for _ in 0..1000 {
                browser.advance_time(1).expect("time should advance");
            }
            let document = browser.document().unwrap();
            let document = document.borrow();
            let count = document.find_element_by_id("count").unwrap();
            document.text_content(count).parse::<u32>().unwrap()
        };
        assert_eq!(frames_in_a_second(None), 62);
        assert_eq!(frames_in_a_second(Some(120_000)), 125);
        assert_eq!(frames_in_a_second(Some(144_000)), 166);
    }

    #[test]
    fn kept_cascades_follow_classes_siblings_hover_and_inline_style() {
        let mut browser = Browser::new(bundle(
            r##"<style>#box { width: 50px; height: 20px } .wide #box { width: 70px }
            #box:hover { width: 90px } p + #box { width: 30px }</style>
            <main id="main"><div id="box"></div></main>"##,
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
        let document = browser.document().unwrap();
        let find = |id: &str| document.borrow().find_element_by_id(id).unwrap();
        let (main, target) = (find("main"), find("box"));
        let width =
            |browser: &Browser| width_of(&browser.layout(viewport).unwrap().root, target).unwrap();
        assert_eq!(width(&browser), 50.0);
        document
            .borrow_mut()
            .set_attribute(main, "class", "wide".to_string());
        assert_eq!(width(&browser), 70.0);
        browser
            .dispatch_at(viewport, 10.0, 10.0, "pointermove")
            .expect("pointer move should dispatch");
        assert_eq!(width(&browser), 90.0);
        document
            .borrow_mut()
            .set_style_property(target, "width", "110px".to_string());
        assert_eq!(width(&browser), 110.0);
        document
            .borrow_mut()
            .set_style_property(target, "width", "120px".to_string());
        assert_eq!(width(&browser), 120.0);
        document.borrow_mut().remove_attribute(target, "style");
        assert_eq!(width(&browser), 90.0);
        browser
            .dispatch_at(viewport, 300.0, 170.0, "pointermove")
            .expect("pointer move should dispatch");
        assert_eq!(width(&browser), 70.0);
        document.borrow_mut().remove_attribute(main, "class");
        assert_eq!(width(&browser), 50.0);
        let paragraph = document.borrow_mut().create_element("p");
        document
            .borrow_mut()
            .insert_before(main, paragraph, Some(target))
            .expect("the paragraph should insert");
        assert_eq!(width(&browser), 30.0);
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
                #status { margin: 0; }
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
    fn font_faces_added_from_script_style_text() {
        let mut resources = bundle(
            r##"<html><body style="margin: 0; font: 20px/30px monospace"><span id="before" style="font-family: 'Script Face', monospace">ABAB</span><br><span id="after" style="font-family: 'Script Face', monospace">ABAB</span><p id="status">idle</p><script>
                const width = (id) => Math.round(document.getElementById(id).getBoundingClientRect().width);
                const fallback = width("before");
                const face = new FontFace("Script Face", "url(/fonts/ceri-test.woff2) format('woff2')");
                const seen = [face.status, document.fonts.has(face), document.fonts.check("20px 'Script Face'")];
                face.load().then((loaded) => {
                    document.fonts.add(loaded);
                    seen.push(loaded === face, face.status, document.fonts.size, document.fonts.has(face), document.fonts.check("20px 'Script Face'"));
                    seen.push(width("after") !== fallback);
                    const broken = new FontFace("Broken", new Uint8Array([1, 2, 3]));
                    seen.push(broken.status);
                    return document.fonts.ready;
                }).then((set) => {
                    seen.push(set === document.fonts, document.fonts.status);
                    document.getElementById("status").textContent = seen.join(",");
                });
            </script></body></html>"##,
        );
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
        browser.advance_time(1).expect("tasks should run");
        let document = browser.document().expect("document should exist");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.element_text_content(status).as_deref(),
            Some("unloaded,false,true,true,loaded,1,true,true,true,error,true,loaded")
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
                .filter(|&x| {
                    (top..top + 30)
                        .any(|y| pixel(x, y).iter().map(|&c| u32::from(c)).sum::<u32>() < 384)
                })
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
                    "upload" => reply.send(Ok(format!(
                        "\"{:?} {} {}\"",
                        call.body,
                        call.header("clip").unwrap_or_default(),
                        call.header("at").unwrap_or_default()
                    ))),
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
                "upload".to_string(),
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
                window.ceri.invoke("upload", new Uint8Array([1, 2, 255]).subarray(1), { headers: { Clip: "a%20b", at: 5 } }).then((reply) => log("upload:" + reply));
                window.ceri.invoke("upload", new Uint8Array([9]).buffer).then((reply) => log("buffer:" + reply));
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
            "main:true|greet:ceri|CeriBridgeError:denied|missing:unknown-command|upload:Some([2, 255]) a%20b 5|buffer:Some([9])  |"
        );
        let calls = router.calls.borrow().clone();
        assert_eq!(calls.len(), 5, "a denied command never reaches the router");
        assert_eq!(calls[0].origin, APP_ORIGIN);
        assert_eq!(calls[0].payload, "{\"name\":\"ceri\"}");
        assert_eq!((&calls[0].body, calls[0].headers.len()), (&None, 0));
        assert_eq!(calls[1].payload, "{}");
        assert_eq!(calls[3].payload, "null");
        assert_eq!(
            calls[3].headers,
            [
                ("clip".to_string(), "a%20b".to_string()),
                ("at".to_string(), "5".to_string())
            ]
        );
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
    fn notifications_go_to_the_host_when_granted_and_fire_show_error_and_close() {
        use crate::bridge::{Capabilities, CommandCall, CommandReply, CommandRouter, Grant};

        #[derive(Default)]
        struct Notifier {
            calls: RefCell<Vec<CommandCall>>,
        }

        impl CommandRouter for Notifier {
            fn invoke(&self, call: CommandCall, reply: CommandReply) {
                self.calls.borrow_mut().push(call);
                reply.send(Ok("null".to_string()));
            }
        }

        let page = r##"<p id="status"></p>
            <script>
                const log = (line) => { document.getElementById("status").textContent += line + "|"; };
                const name = (run) => { try { run(); return "none"; } catch (error) { return error.name; } };
                log([Notification.permission, Notification.maxActions, Notification.length].join(","));
                Notification.requestPermission((permission) => log("asked:" + permission)).then((permission) => log("granted:" + permission));
                log([
                    name(() => new Notification("a", { actions: [{ action: "a", title: "b" }] })),
                    name(() => new Notification("a", { renotify: true })),
                    name(() => new Notification("a", { dir: "up" })),
                    name(() => Notification("a")),
                    name(() => Notification.prototype.close.call({})),
                ].join(","));
                const first = new Notification("Hi", { body: "b1", tag: "t", data: { n: 1 }, icon: "/i.png", dir: "rtl", timestamp: 5.9 });
                log([first.title, first.body, first.tag, first.dir, first.icon, first.silent, first.data.n, first.data === first.data, first.actions.length, first.timestamp, first instanceof EventTarget].join(","));
                first.onerror = () => log("error1");
                first.onclose = () => log("close1");
                first.onshow = () => {
                    log("show1");
                    const second = new Notification("Again", { tag: "t", silent: false });
                    second.addEventListener("show", () => { log("show2:" + second.silent); second.close(); second.close(); });
                    second.onclose = () => log("close2");
                };
            </script>"##;
        let notices = |granted: &[&str]| {
            let router = Rc::new(Notifier::default());
            let mut browser =
                Browser::new(bundle(page)).with_host_bridge(crate::script::HostBridge::new(
                    router.clone(),
                    Capabilities::new(vec![Grant {
                        windows: vec!["main".to_string()],
                        origins: vec![APP_ORIGIN.to_string()],
                        commands: granted.iter().map(|command| command.to_string()).collect(),
                    }]),
                    "main",
                ));
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("page should load");
            for _ in 0..4 {
                browser.pump().expect("pump");
            }
            let document = browser.document().expect("document");
            let document = document.borrow();
            let status = document.find_element_by_id("status").expect("status");
            let status = document.element_text_content(status).unwrap_or_default();
            let payloads: Vec<String> = router
                .calls
                .borrow()
                .iter()
                .map(|call| format!("{}:{}", call.command, call.payload))
                .collect();
            (status, payloads)
        };
        let checks = "TypeError,TypeError,TypeError,TypeError,TypeError|Hi,b1,t,rtl,https://app.ceri.invalid/i.png,,1,false,0,5,true|";
        let (status, payloads) = notices(&["notification:*"]);
        assert_eq!(
            status,
            format!(
                "granted,0,1|asked:granted|{checks}granted:granted|show1|show2:false|close1|close2|"
            )
        );
        assert_eq!(
            payloads,
            [
                "notification:show:{\"title\":\"Hi\",\"body\":\"b1\"}",
                "notification:show:{\"title\":\"Again\",\"body\":\"\"}"
            ]
        );
        let (status, payloads) = notices(&["greet"]);
        assert_eq!(
            status,
            format!("denied,0,1|asked:denied|{checks}granted:denied|error1|")
        );
        assert!(
            payloads.is_empty(),
            "a denied notice never reaches the host"
        );
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
    fn browser_moves_scrolled_pixels_instead_of_painting_them_again() {
        let mut browser = Browser::new(bundle(
            r##"<style>
                body { margin: 0; background: #121315; color: #e8e8e8 }
                #list { height: 50px; overflow: auto; margin: 6px; border-radius: 5px }
                #list p { height: 15px; margin: 0; background: linear-gradient(#202124, #34353a) }
                #bar { position: fixed; bottom: 2px; left: 4px; width: 50px; height: 10px; background: #345 }
                #tall { height: 300px }
            </style>
            <div id="list"><p>one</p><p>two</p><p>three</p><p>four</p><p>five</p><p>six</p></div>
            <div id="tall">rest of the page</div><div id="bar"></div>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let viewport = Size {
            width: 120.0,
            height: 100.0,
        };
        browser.paint(viewport).expect("the page should paint");
        for (x, y, delta) in [
            (20.0, 20.0, 12.0),
            (20.0, 20.0, -5.0),
            (90.0, 80.0, 40.0),
            (90.0, 80.0, -27.0),
        ] {
            let (generation, layout, shown) = browser
                .painted
                .borrow()
                .as_ref()
                .map(|last| (last.generation, last.layout.clone(), last.canvas.clone()))
                .expect("a frame was painted");
            assert!(
                browser
                    .scroll_at(viewport, x, y, 0.0, delta)
                    .expect("the wheel should dispatch")
            );
            let canvas = browser
                .paint(viewport)
                .expect("the scrolled page should paint");
            let scrolled = browser.painted.take().expect("the frame was kept");
            let whole = browser
                .paint(viewport)
                .expect("the page should paint whole");
            assert_eq!(
                scrolled.generation, generation,
                "scrolling {delta} at {x},{y} laid the page out again"
            );
            assert!(
                crate::paint::scroll_repaint(&shown, &layout, &scrolled.layout).is_some(),
                "scrolling {delta} at {x},{y} painted again instead of moving pixels"
            );
            browser.painted.replace(Some(scrolled));
            assert!(
                canvas
                    .rgba_pixels()
                    .iter()
                    .zip(whole.rgba_pixels())
                    .all(|(a, b)| a.abs_diff(*b) <= 1),
                "scrolling {delta} at {x},{y} differs from a whole frame"
            );
        }
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
    fn cross_origin_module_imports_are_refused_and_reported() {
        let mut resources =
            bundle(r#"<p id="before">before</p><script type="module" src="/entry.js"></script>"#);
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
                .expect("the page should load"),
            NavigationStatus::Loaded
        );
        let document = browser.document().expect("document should exist");
        assert!(document.borrow().find_element_by_id("before").is_some());
        let logs = browser.console_logs().join("\n");
        assert!(logs.contains("Uncaught"), "{logs}");
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
    fn urls_print_back_to_what_they_parse_from() {
        let clip =
            AppUrl::parse("https://files.ceri.invalid/home/ana/Clip%2019%3A42%20%2325%25.mp4")
                .expect("a files path may hold an escaped colon");
        assert_eq!(clip.path(), "/home/ana/Clip 19:42 #25%.mp4");
        assert_eq!(
            clip.to_string(),
            "https://files.ceri.invalid/home/ana/Clip 19%3A42 %2325%25.mp4"
        );
        assert_eq!(AppUrl::parse(&clip.to_string()).expect("reparses"), clip);
        let sibling = clip
            .resolve("cover%231.png?v=2")
            .expect("relative reference");
        assert_eq!(sibling.path(), "/home/ana/cover#1.png");
        assert_eq!(sibling.query(), "v=2");
        assert!(matches!(
            AppUrl::parse("https://files.ceri.invalid/home/a:b"),
            Err(NavigationError::Colon)
        ));
        let module = AppUrl::parse("https://app.ceri.invalid/@fs/D%3A/web/deps.js?v=1")
            .expect("an app path may hold a colon");
        assert_eq!(module.path(), "/@fs/D:/web/deps.js");
        assert_eq!(
            module.to_string(),
            "https://app.ceri.invalid/@fs/D:/web/deps.js?v=1"
        );
        assert_eq!(
            AppUrl::parse(&module.to_string()).expect("reparses"),
            module
        );
        assert_eq!(
            module.resolve("./chunk.js").expect("sibling").path(),
            "/@fs/D:/web/chunk.js"
        );
        let page = AppUrl::parse("https://app.ceri.invalid/notes/100%25%23.html").expect("url");
        assert_eq!(page.path(), "/notes/100%#.html");
        assert_eq!(AppUrl::parse(&page.to_string()).expect("reparses"), page);
        assert_eq!(page.resolve("").expect("same document"), page);
    }

    #[test]
    fn granted_files_load_as_subresources_and_never_as_documents() {
        struct Granted;
        impl ResourceProvider for Granted {
            fn provide(&self, url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
                Ok((url.is_files() && url.path() == "/shots/note.txt")
                    .then(|| Resource::new("text/plain", b"from the grant".to_vec())))
            }
        }

        let url = AppUrl::parse("https://FILES.ceri.invalid/shots/a%20b.png?v=2")
            .expect("files URL should parse");
        assert!(url.is_files());
        assert_eq!(url.path(), "/shots/a b.png");
        assert_eq!(url.origin(), FILES_ORIGIN);
        assert_eq!(
            url.to_string(),
            "https://files.ceri.invalid/shots/a b.png?v=2"
        );
        let sibling = url
            .resolve("../art/c.png")
            .expect("relative URL should resolve");
        assert!(sibling.is_files());
        assert_eq!(sibling.path(), "/art/c.png");
        assert!(matches!(
            external_url("https://files.ceri.invalid/shots/a.png"),
            Err(NavigationError::WrongOrigin)
        ));

        // the bundle's file at the same path must not answer for the files origin
        let mut resources = bundle(
            r#"<p id="status">idle</p><script>fetch("https://files.ceri.invalid/shots/note.txt").then((response) => response.text()).then((body) => { document.getElementById("status").textContent = body + "|" + new URL("https://files.ceri.invalid/x").origin + "|" + location.origin; });</script>"#,
        );
        resources
            .insert("/shots/note.txt", "text/plain", b"from the bundle".to_vec())
            .expect("decoy resource should be accepted");
        assert!(
            resources
                .resource_at(&url.with_path("/shots/note.txt").unwrap())
                .is_none()
        );
        let mut browser = Browser::with_resource_provider(resources, Rc::new(Granted));
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
            Some("from the grant|https://files.ceri.invalid|https://app.ceri.invalid")
        );
        assert_eq!(
            browser.navigate("https://files.ceri.invalid/shots/note.txt"),
            Err(NavigationError::WrongOrigin)
        );
    }

    #[test]
    fn missing_stylesheets_are_left_out() {
        let mut browser = Browser::new(bundle(
            r#"<link rel="stylesheet" href="/assets/missing.css"><p id="home">home</p>"#,
        ));
        assert_eq!(
            browser
                .navigate("https://app.ceri.invalid/")
                .expect("the page should load"),
            NavigationStatus::Loaded
        );
        let document = browser.document().expect("document should exist");
        assert!(document.borrow().find_element_by_id("home").is_some());
        let logs = browser.console_logs().join("\n");
        assert!(logs.contains("stylesheet failed to load"), "{logs}");
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
    fn canvas_bitmaps_paint_at_their_natural_size() {
        let mut browser = Browser::new(bundle(
            r##"<body style="margin: 0"><canvas id="art" width="40" height="30">fallback text</canvas><script>
                const ctx = document.getElementById("art").getContext("2d");
                ctx.fillStyle = "#ff0000";
                ctx.fillRect(0, 0, 40, 30);
            </script></body>"##,
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
        let pixel = |x: usize, y: usize| canvas.rgba_pixels()[(y * 160 + x) * 4..][..4].to_vec();
        assert_eq!(pixel(0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(39, 29), [255, 0, 0, 255]);
        assert_ne!(pixel(40, 10), [255, 0, 0, 255]);
        assert_ne!(pixel(10, 30), [255, 0, 0, 255]);
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
                .as_chunks::<4>()
                .0
                .contains(&[110, 231, 197, 255])
        );
        assert!(
            canvas
                .rgba_pixels()
                .as_chunks::<4>()
                .0
                .contains(&[224, 108, 117, 255])
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

    #[test]
    fn a_half_pixel_translate_draws_svg_vectors_at_the_moved_spot_without_resampling() {
        let mut browser = Browser::new(bundle(
            r##"<body style="margin: 0; background: #000"><svg width="4" height="4" style="display: block; translate: 0 0.5px"><rect y="0.5" width="4" height="1" fill="#fff"/></svg></body>"##,
        ));
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("svg page should load");
        let canvas = browser
            .paint(Size {
                width: 8.0,
                height: 8.0,
            })
            .expect("svg page should paint");
        assert_eq!(pixel_at(&canvas, 1, 0), [0, 0, 0, 255]);
        assert_eq!(pixel_at(&canvas, 1, 1), [255, 255, 255, 255]);
        assert_eq!(pixel_at(&canvas, 1, 2), [0, 0, 0, 255]);
    }

    #[test]
    fn text_in_a_rounded_modal_is_grayscale_and_in_a_rounded_fixed_box_from_a_dense_display() {
        let mut resources = bundle(
            r##"<html><head><style>
                @font-face { font-family: "Ceri Web"; src: url(/fonts/ceri-test.woff2) format("woff2") }
                body { margin: 0; background: #000; color: #fff; font: 17px/40px "Ceri Web" }
                .card { left: 0; width: 200px; height: 40px; margin: 0; padding: 0; border: 0; background: #222; color: #fff; border-radius: 6px }
            </style></head><body>
            <p style="margin: 0; height: 40px">ABAB fi AB</p><p style="margin: 0; font-size: 0">ABAB fi AB</p>
            <div class="card" style="position: fixed; top: 40px">ABAB fi AB</div>
            <dialog id="box" class="card" style="top: 80px">ABAB fi AB</dialog>
            <script>document.getElementById("box").showModal();</script></body></html>"##,
        );
        let font = std::fs::read(format!(
            "{}/tests/fixtures/fonts/ceri-test.woff2",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("fixture font should exist");
        resources
            .insert("/fonts/ceri-test.woff2", "font/ttf", font)
            .expect("font should be accepted");
        let mut browser = Browser::new(resources);
        browser
            .navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let viewport = Size {
            width: 240.0,
            height: 130.0,
        };
        // pixels in a band of the page, by device scale, whose channels differ: the fringes
        // of text blended per subpixel
        let fringes = |scale: f32, band: u32| {
            let canvas = browser.paint_scaled(viewport, scale).expect("page paints");
            let rows =
                (band as f32 * 40.0 * scale) as u32..((band + 1) as f32 * 40.0 * scale) as u32;
            rows.flat_map(|y| (0..canvas.width()).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let [red, green, blue, _] = pixel_at(&canvas, x, y);
                    red.abs_diff(green).max(green.abs_diff(blue)) > 24
                })
                .count()
        };
        // only directwrite rasterizes per subpixel, so elsewhere all text is grayscale
        if cfg!(windows) {
            assert!(fringes(1.0, 0) > 0, "page text blends per subpixel");
            assert!(fringes(1.0, 1) > 0, "a fixed box is no layer at 1x");
            assert!(fringes(1.5, 0) > 0);
        }
        assert_eq!(fringes(1.0, 2), 0, "a modal is a layer at any scale");
        assert_eq!(fringes(1.5, 1), 0, "a fixed box is a layer from 1.5x");
        assert_eq!(fringes(1.5, 2), 0);
    }

    fn image_bundle(source: &str) -> ResourceBundle {
        let mut resources = bundle(source);
        for name in [
            "rgb8.png",
            "rgba8.png",
            "baseline-420.jpg",
            "frame.gif",
            "animated.gif",
        ] {
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

    fn image_page(
        source: &str,
        width: f32,
        height: f32,
    ) -> (Browser, Rc<LayoutDocument>, Arc<Canvas>) {
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
    fn animated_images_show_the_frame_the_clock_reaches() {
        let bytes = std::fs::read(format!(
            "{}/tests/fixtures/animated.gif",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("fixture");
        let first = Arc::new(crate::image::decode_image(&bytes, "image/gif").expect("first frame"));
        let mut animation =
            crate::animated::Animation::new(&bytes, first.clone(), 0).expect("an animation");
        let delay = animation.next_change(0).expect("a second frame comes");
        let second = animation.frame_at(delay);
        // a pixel the second frame changes to an opaque colour
        let (x, y) = (0..first.height)
            .flat_map(|y| (0..first.width).map(move |x| (x, y)))
            .find(|&(x, y)| {
                second.pixel(x, y) != first.pixel(x, y) && second.pixel(x, y).alpha == 255
            })
            .expect("the frames differ");
        let (mut browser, _, before) = image_page(
            "<style>body { margin: 0 } img { display: block }</style><img src=\"/animated.gif\">",
            100.0,
            100.0,
        );
        let viewport = Size {
            width: 100.0,
            height: 100.0,
        };
        assert!(browser.next_wake().is_some_and(|wake| wake <= delay));
        browser.advance_time(delay).expect("time should advance");
        let after = browser.paint(viewport).expect("page should paint");
        let want = second.pixel(x, y);
        assert_ne!(pixel_at(&before, x, y), pixel_at(&after, x, y));
        assert_eq!(
            pixel_at(&after, x, y),
            [want.red, want.green, want.blue, 255]
        );
    }

    #[test]
    fn object_position_moves_the_fitted_picture() {
        let (_, _, canvas) = image_page(
            "<style>body { margin: 0; } img { display: block; width: 26px; height: 26px; }</style><img src=\"/rgb8.png\" style=\"object-fit: contain; object-position: top\"><img src=\"/rgb8.png\" style=\"object-fit: contain; object-position: 0 100%\"><img src=\"/rgb8.png\" style=\"object-fit: none; object-position: 2px 3px\">",
            100.0,
            80.0,
        );
        let blank = pixel_at(&canvas, 99, 79);
        // contain makes the picture 26 by 14, at the top of the first box and the bottom of
        // the second
        assert_ne!(pixel_at(&canvas, 13, 1), blank);
        assert_eq!(pixel_at(&canvas, 13, 20), blank);
        assert_eq!(pixel_at(&canvas, 13, 26 + 5), blank);
        assert_ne!(pixel_at(&canvas, 13, 26 + 20), blank);
        // none keeps the natural size, shifted by the lengths
        assert_eq!(pixel_at(&canvas, 2 + 4, 52 + 3 + 5), fixture_color(4, 5));
        assert_eq!(pixel_at(&canvas, 1, 52 + 1), blank);
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
