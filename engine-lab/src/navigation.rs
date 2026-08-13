use crate::css::Stylesheet;
use crate::dom::{AccessibilityNode, Document, NodePath, ScriptSource, StylesheetSource};
use crate::image::parse_svg;
use crate::layout::{
    ImageLayer, LayoutDocument, Size, layout_with_state as build_layout,
};
use crate::paint::Canvas;
use crate::script::{
    HostRuntimeInfo, ModuleSourceLoader, NavigationRequest, ScriptRuntime, StorageHandle,
    runtime_error,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

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

    pub fn with_query_fragment(&self, query: impl Into<String>, fragment: impl Into<String>) -> Self {
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

pub trait ResourceProvider {
    fn provide(&self, url: &AppUrl) -> Result<Option<Resource>, NavigationError>;

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
        collect_directory(&root, &root, &mut resources, &mut visited)?;
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
                write!(formatter, "resource request body is not allowed for {method}")
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
            Self::ResourceProvider(detail) => write!(formatter, "resource provider failed: {detail}"),
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
        NavigationError::ResourceProvider(_) => 503,
        _ => 500,
    }
}

fn load_provider_resource(
    provider: &dyn ResourceProvider,
    url: &AppUrl,
) -> Result<Option<(AppUrl, Resource)>, NavigationError> {
    load_provider_resource_at(provider, url, 0)
}

fn load_provider_resource_at(
    provider: &dyn ResourceProvider,
    url: &AppUrl,
    redirect_depth: usize,
) -> Result<Option<(AppUrl, Resource)>, NavigationError> {
    let Some(response) = provider.provide_response(&ResourceRequest::get(url.clone()))? else {
        return Ok(None);
    };
    if is_http_redirect(response.status()) {
        if redirect_depth >= MAX_NAVIGATION_REDIRECTS {
            return Err(NavigationError::NavigationLoop);
        }
        let Some(location) = response.headers().get("location") else {
            return Err(NavigationError::HttpStatus {
                path: url.path().to_string(),
                status: response.status(),
            });
        };
        let target = url.resolve(location)?;
        return load_provider_resource_at(provider, &target, redirect_depth + 1);
    }
    if !(200..300).contains(&response.status()) {
        return Err(NavigationError::HttpStatus {
            path: url.path().to_string(),
            status: response.status(),
        });
    }
    Ok(Some((
        url.clone(),
        Resource::new(response.mime_type().to_string(), response.body().to_vec()),
    )))
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
    stylesheet: String,
}

enum HistoryCommit {
    Push { state: String },
    Replace { state: String },
    Traverse { index: usize, state: String },
}

pub struct Browser {
    resources: ResourceBundle,
    resource_provider: Option<Rc<dyn ResourceProvider>>,
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
    hovered_path: Option<NodePath>,
    host_runtime_info: Option<HostRuntimeInfo>,
}

impl Browser {
    pub fn new(resources: ResourceBundle) -> Self {
        Self {
            resources,
            resource_provider: None,
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
            hovered_path: None,
            host_runtime_info: None,
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

    pub fn with_host_runtime_info(mut self, info: HostRuntimeInfo) -> Self {
        self.host_runtime_info = Some(info);
        self
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
        let LoadedResource {
            url,
            resource,
        } = match self.load_resource(&url) {
            Ok(resource) => resource,
            Err(NavigationError::ResourceNotFound(path)) => {
                self.install_error_page(&url, 404, &format!("resource not found: {path}"));
                self.commit_new_document(url, commit);
                return Ok(NavigationStatus::ErrorPage { status: 404 });
            }
            Err(error @ NavigationError::HttpStatus { .. }) => {
                let status = recovery_status(&error);
                self.install_error_page(&url, status, &error.to_string());
                self.commit_new_document(url, commit);
                return Ok(NavigationStatus::ErrorPage { status });
            }
            Err(error @ NavigationError::ResourceProvider(_)) => {
                self.install_error_page(&url, 503, &error.to_string());
                self.commit_new_document(url, commit);
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
        if let Err(error) = self.execute_scripts(&url, &script_sources, &mut runtime) {
            let status = recovery_status(&error);
            self.install_error_page(&url, status, &error.to_string());
            self.commit_new_document(url, commit);
            return Ok(NavigationStatus::ErrorPage { status });
        }

        let pending_navigation = runtime.take_navigation_request();
        self.page = Some(Page {
            document,
            runtime,
            stylesheet,
        });
        self.last_navigation_failure = None;
        self.hovered_path = None;
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
        let path = {
            let page = self.page.as_ref().ok_or(NavigationError::NoPage)?;
            page.document.borrow().find_element_by_id(id)
        };
        let Some(path) = path else {
            return Ok(false);
        };
        self.activate_path(path)
    }

    pub fn click_at(&mut self, viewport: Size, x: f32, y: f32) -> Result<bool, NavigationError> {
        let path = self.layout(viewport)?.hit_test(x, y);
        let Some(path) = path else {
            return Ok(false);
        };
        self.activate_path(path)
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
            let changed = page
                .runtime
                .set_scroll_offset(target.path.clone(), offset);
            if changed {
                if target.path.is_empty() {
                    page.runtime
                        .dispatch_window_event("scroll")
                        .map_err(|error| NavigationError::Script(error.to_string()))?;
                } else {
                    page.runtime
                        .dispatch_path_event(target.path, "scroll")
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

    pub fn dispatch_at(
        &mut self,
        viewport: Size,
        x: f32,
        y: f32,
        event: &str,
    ) -> Result<bool, NavigationError> {
        let path = self.layout(viewport)?.hit_test(x, y);
        let hover_changed = if event == "pointermove" && self.hovered_path != path {
            self.hovered_path = path.clone();
            true
        } else {
            false
        };
        let Some(path) = path else {
            return Ok(hover_changed);
        };
        let handled = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            let focused = if event == "pointerdown" {
                page.runtime
                    .focus_path(path.clone())
                    .map_err(|error| NavigationError::Script(error.to_string()))?
            } else {
                false
            };
            let dispatched = page
                .runtime
                .dispatch_path_event(path, event)
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
        let handled = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            page.runtime
                .key_down(key)
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

    pub fn layout(&self, viewport: Size) -> Result<LayoutDocument, NavigationError> {
        let page = self.page.as_ref().ok_or(NavigationError::NoPage)?;
        let document = page.document.clone();
        let document = document.borrow();
        let stylesheet =
            Stylesheet::parse(&page.stylesheet).map_err(NavigationError::Stylesheet)?;
        let mut style_state = page.runtime.style_state(self.hovered_path.clone());
        style_state.viewport_width = viewport.width;
        style_state.viewport_height = viewport.height;
        let mut layout = build_layout(&document, &stylesheet, viewport, &style_state);
        if let Some(base) = self.current_url.as_ref() {
            self.attach_image_layers(base, &document, &mut layout);
        }
        Ok(layout)
    }

    pub fn paint(&self, viewport: Size) -> Result<Canvas, NavigationError> {
        let layout = self.layout(viewport)?;
        let commands = self
            .page
            .as_ref()
            .map(|page| page.runtime.canvas_commands())
            .unwrap_or(&[]);
        Ok(crate::paint::paint_with_commands(&layout, commands))
    }

    pub fn describe(&self, viewport: Size) -> Result<String, NavigationError> {
        Ok(self.layout(viewport)?.describe())
    }

    fn attach_image_layers(
        &self,
        base: &AppUrl,
        document: &Document,
        layout: &mut LayoutDocument,
    ) {
        let mut paths = Vec::new();
        collect_image_paths(&layout.root, &mut paths);
        for path in paths {
            let Some(reference) = document.element_attribute(&path, "src") else {
                continue;
            };
            let Ok(url) = base.resolve(&reference) else {
                continue;
            };
            let Ok(resource) = self.load_subresource(&url) else {
                continue;
            };
            if !is_svg(&resource) {
                continue;
            }
            let Ok(source) = std::str::from_utf8(resource.body()) else {
                continue;
            };
            let Ok(image) = parse_svg(source) else {
                continue;
            };
            layout.images.push(ImageLayer { path, image });
        }
    }

    fn activate_path(&mut self, path: Vec<usize>) -> Result<bool, NavigationError> {
        let handled = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            page.runtime
                .click_path(path)
                .map_err(|error| NavigationError::Script(error.to_string()))?
        };
        if self.process_pending_navigation()?.is_some() {
            Ok(true)
        } else {
            Ok(handled)
        }
    }

    fn process_pending_navigation(
        &mut self,
    ) -> Result<Option<NavigationStatus>, NavigationError> {
        let request = {
            let page = self.page.as_mut().ok_or(NavigationError::NoPage)?;
            page.runtime.take_navigation_request()
        };
        request
            .map(|request| self.follow_navigation(request, 0))
            .transpose()
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
                StylesheetSource::Inline(source) => source.clone(),
                StylesheetSource::External(reference) => {
                    let url = base.resolve(reference)?;
                    let resource = self.load_subresource(&url)?;
                    if !is_css(&resource) {
                        return Err(NavigationError::UnsupportedMimeType {
                            path: url.path().to_string(),
                            mime_type: resource.mime_type,
                        });
                    }
                    String::from_utf8(resource.body)
                        .map_err(|_| NavigationError::InvalidUtf8(url.path().to_string()))?
                }
            };
            if !stylesheet.is_empty() {
                stylesheet.push('\n');
            }
            stylesheet.push_str(&source);
        }
        Ok(stylesheet)
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
        let stylesheet = document.borrow().style_text();
        self.page = Some(Page {
            runtime: ScriptRuntime::with_storage_and_runtime_info(
                document.clone(),
                self.local_storage.clone(),
                self.session_storage.clone(),
                self.host_runtime_info.clone(),
            ),
            document,
            stylesheet,
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
                let Some((loaded_url, resource)) =
                    load_provider_resource(provider, &url)
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

fn is_svg(resource: &Resource) -> bool {
    resource
        .mime_type
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("image/svg+xml"))
}

fn collect_image_paths(layout: &crate::layout::LayoutBox, paths: &mut Vec<NodePath>) {
    if layout.tag == "img" {
        paths.push(layout.path.clone());
    }
    for child in &layout.children {
        collect_image_paths(child, paths);
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

fn collect_directory(
    root: &Path,
    directory: &Path,
    resources: &mut ResourceBundle,
    visited: &mut BTreeSet<PathBuf>,
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
            collect_directory(root, &path, resources, visited)?;
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

fn mime_type_for(path: &Path) -> &'static str {
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
        if value.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0..=31 | 127)) {
            return Err(NavigationError::InvalidRequestHeader(name));
        }
        normalized.insert(name, value);
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::Color;
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
            Ok((url.path() == "/index.html").then(|| {
                Resource::new("text/html", b"<p id='status'>shell</p>".to_vec())
            }))
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

    fn bundle(shell: &str) -> ResourceBundle {
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/index.html",
                "text/html; charset=utf-8",
                shell.as_bytes().to_vec(),
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
        assert!(resources.resource("/target/debug/engine-lab.exe").is_none());
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
                br#"self.onmessage = (event) => { postMessage(event.data + "!"); };"#
                    .to_vec(),
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
            document.borrow().element_text_content(&status).as_deref(),
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
                br#"self.onmessage = (event) => { postMessage(event.data + "!"); };"#
                    .to_vec(),
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
            document.borrow().element_text_content(&status).as_deref(),
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
            document.borrow().element_text_content(&status).as_deref(),
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
            Resource::new("text/javascript", b"importScripts('./worker-helper.js');".to_vec()),
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
            document.borrow().element_text_content(&status).as_deref(),
            Some("classic!")
        );
    }

    #[test]
    fn browser_instantiates_provider_wasm_without_a_loopback_server() {
        let wasm = vec![
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x07, 0x01, 0x60, 0x02,
            0x7f, 0x7f, 0x01, 0x7f, 0x03, 0x02, 0x01, 0x00, 0x07, 0x07, 0x01, 0x03, 0x61,
            0x64, 0x64, 0x00, 0x00, 0x0a, 0x09, 0x01, 0x07, 0x00, 0x20, 0x00, 0x20, 0x01,
            0x6a, 0x0b,
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
                (document.element_text_content(&status).as_deref() == Some("5")).then_some(())
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
            document.borrow().element_text_content(&status).as_deref(),
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
            browser
                .document()
                .unwrap()
                .borrow()
                .element_text_content(&[0])
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
        assert!(browser.click_id("label").expect("nested link should activate"));
        assert_eq!(browser.current_url().unwrap().path(), "/about.html");
        assert_eq!(browser.history().len(), 2);
        let document = browser.document().expect("about document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("about")
        );
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
            document.borrow().element_text_content(&status).as_deref(),
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
        assert!(browser.click_id("go").expect("location replace should navigate"));
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
        assert!(browser.click_id("go").expect("location assignment should navigate"));
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
            &browser.document().expect("SPA document should remain loaded")
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
                .element_text_content(&status)
                .as_deref(),
            Some("/details|details/2")
        );
        assert!(browser.click_id("back").expect("history.back should run"));
        assert_eq!(browser.current_url().unwrap().path(), "/");
        assert!(Rc::ptr_eq(
            &home_document,
            &browser.document().expect("SPA document should remain loaded")
        ));
        assert_eq!(
            browser
                .document()
                .expect("SPA document should exist")
                .borrow()
                .element_text_content(&status)
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
                .element_text_content(&status)
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
        assert!(browser
            .click_id("replace")
            .expect("replaceState should run"));
        assert_eq!(browser.current_url().unwrap().path(), "/settings");
        assert_eq!(browser.history().len(), 1);
        let document = browser.document().expect("SPA document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("settings/1")
        );
    }

    #[test]
    fn history_traversal_reloads_different_documents() {
        let mut resources = bundle(
            r#"<a id="about" href="/about.html">about</a><p id="status">home</p>"#,
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
        assert!(browser.click_id("about").expect("about link should activate"));
        assert_eq!(browser.current_url().unwrap().path(), "/about.html");
        assert!(browser.back().expect("history back should run"));
        assert_eq!(browser.current_url().unwrap().path(), "/");
        let document = browser.document().expect("home document should reload");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
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
            document.borrow().element_text_content(&status).as_deref(),
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
            document.borrow().element_text_content(&status).as_deref(),
            Some("ready|open")
        );
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
        assert!(browser.click_id("submit").expect("submit click should dispatch"));
        assert_eq!(browser.current_url().unwrap().path(), "/results.html");
        assert_eq!(browser.current_url().unwrap().query(), "q=rust+browser");
        let document = browser.document().expect("results document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("?q=rust+browser")
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
            document.borrow().element_text_content(&[0]).as_deref(),
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
                .element_attribute(&recovery, "data-status"),
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
        assert_eq!(failure.detail, "resource provider failed: transport unavailable");
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
        let document = browser.document().expect("redirected document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status element should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
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
            document.borrow().element_text_content(&status).as_deref(),
            Some("redirected")
        );
    }

    #[test]
    fn provider_shell_fallback_preserves_the_requested_app_url() {
        let mut browser = Browser::with_resource_provider(
            ResourceBundle::default(),
            Rc::new(ShellProvider),
        );
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
                .element_attribute(&recovery, "data-status"),
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
            document.borrow().element_text_content(&status).as_deref(),
            Some("clicked")
        );
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
            document.borrow().element_text_content(&status).as_deref(),
            Some("overlay clicked")
        );
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
        assert!(browser
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
            .expect("wheel scroll should dispatch"));
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("scroll:30:0")
        );
        let layout = browser
            .layout(Size {
                width: 120.0,
                height: 100.0,
            })
            .expect("scrolled page should layout");
        assert_eq!(layout.root.children[0].children[0].rect.y, -30.0);
        assert!(browser
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
            .expect("scroll should clamp to the content boundary"));
        assert!(!browser
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
            .expect("scroll at the boundary should be ignored"));
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
        assert!(browser
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
            .expect("document wheel scroll should dispatch"));
        let document = browser.document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("window:30:30")
        );
        let layout = browser
            .layout(Size {
                width: 120.0,
                height: 100.0,
            })
            .expect("scrolled document should layout");
        assert_eq!(layout.root.children[0].rect.y, -30.0);
        assert_eq!(layout.root.children[1].rect.y, 0.0);
        assert!(browser
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
            .expect("document scroll should clamp to the page boundary"));
        assert!(!browser
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
            .expect("document scroll at the boundary should be ignored"));
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
            document.borrow().element_text_content(&status).as_deref(),
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
            document.borrow().element_text_content(&created).as_deref(),
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
            document.borrow().element_text_content(&status).as_deref(),
            Some("external script ready")
        );
        let layout = browser
            .layout(Size {
                width: 320.0,
                height: 120.0,
            })
            .expect("page should layout");
        assert_eq!(
            layout.root.children[0].text[0].color,
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
            Resource::new("text/javascript", br#"export const label = "provider";"#.to_vec()),
        );
        provided.insert(
            "/data.json".to_string(),
            Resource::new("application/json", br#"{"message":"ready"}"#.to_vec()),
        );

        let provider = Rc::new(MemoryProvider { resources: provided });
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
            document.borrow().element_text_content(&status).as_deref(),
            Some("provider|ready")
        );
        let layout = browser
            .layout(Size {
                width: 320.0,
                height: 120.0,
            })
            .expect("provider-backed page should layout");
        assert_eq!(
            layout.root.children[0].text[0].color,
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
            document.borrow().element_text_content(&status).as_deref(),
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
        let mut resources = bundle(
            r#"<p id="status">idle</p><script type="module" src="/entry.js"></script>"#,
        );
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
            document.borrow().element_text_content(&status).as_deref(),
            Some("from module ready|from module")
        );
    }

    #[test]
    fn cross_origin_module_imports_install_the_recovery_document() {
        let mut resources = bundle(
            r#"<p>before</p><script type="module" src="/entry.js"></script>"#,
        );
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
            document.borrow().element_attribute(&recovery, "data-status"),
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
            document.borrow().element_text_content(&status).as_deref(),
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
                .element_attribute(&recovery, "data-status"),
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
            narrow.root.children[0].text[0].color,
            Color::opaque(255, 0, 0)
        );
        let wide = browser
            .layout(Size {
                width: 600.0,
                height: 200.0,
            })
            .expect("wide layout should build");
        assert_eq!(
            wide.root.children[0].text[0].color,
            Color::opaque(0, 0, 255)
        );
        let short = browser
            .layout(Size {
                width: 400.0,
                height: 80.0,
            })
            .expect("short layout should build");
        assert_eq!(
            short.root.children[0].text[0].color,
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
        assert!(canvas
            .rgba_pixels()
            .chunks_exact(4)
            .any(|pixel| pixel == [255, 0, 0, 255]));
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
        assert!(canvas
            .rgba_pixels()
            .chunks_exact(4)
            .any(|pixel| pixel == [110, 231, 197, 255]));
        assert!(canvas
            .rgba_pixels()
            .chunks_exact(4)
            .any(|pixel| pixel == [224, 108, 117, 255]));
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
                button { display: block; width: 120px; height: 40px; background: #111111; }
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
        assert_eq!(initial.root.children[0].background, Color::opaque(17, 17, 17));
        assert_eq!(initial.root.children[0].border_width, 0.0);

        assert!(browser
            .dispatch_at(
                Size {
                    width: 240.0,
                    height: 180.0,
                },
                20.0,
                20.0,
                "pointermove",
            )
            .expect("pointer move should update hover"));
        let hovered = browser
            .layout(Size {
                width: 240.0,
                height: 180.0,
            })
            .expect("hovered layout should build");
        assert_eq!(hovered.root.children[0].background, Color::opaque(255, 0, 0));

        assert!(browser
            .click_at(
                Size {
                    width: 240.0,
                    height: 180.0,
                },
                20.0,
                20.0,
            )
            .expect("button click should focus"));
        let focused = browser
            .layout(Size {
                width: 240.0,
                height: 180.0,
            })
            .expect("focused layout should build");
        assert_eq!(focused.root.children[0].border_width, 3.0);

        assert!(browser.click_id("check").expect("checkbox should click"));
        let checked = browser
            .layout(Size {
                width: 240.0,
                height: 180.0,
            })
            .expect("checked layout should build");
        assert_eq!(checked.root.children[1].background, Color::opaque(0, 0, 255));
    }
}
