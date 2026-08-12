use crate::css::Stylesheet;
use crate::dom::{Document, ScriptSource, StylesheetSource};
use crate::layout::{LayoutDocument, Size, layout as build_layout};
use crate::paint::Canvas;
use crate::script::ScriptRuntime;
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
    ResourceNotFound(String),
    UnsupportedMimeType { path: String, mime_type: String },
    InvalidUtf8(String),
    Script(String),
    Stylesheet(String),
    NoPage,
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
            Self::ResourceNotFound(path) => write!(formatter, "resource not found: {path}"),
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
        }
    }
}

impl std::error::Error for NavigationError {}

struct Page {
    document: Rc<RefCell<Document>>,
    runtime: ScriptRuntime,
    stylesheet: String,
}

pub struct Browser {
    resources: ResourceBundle,
    page: Option<Page>,
    current_url: Option<AppUrl>,
    history: Vec<AppUrl>,
}

impl Browser {
    pub fn new(resources: ResourceBundle) -> Self {
        Self {
            resources,
            page: None,
            current_url: None,
            history: Vec::new(),
        }
    }

    pub fn navigate(&mut self, input: &str) -> Result<NavigationStatus, NavigationError> {
        let url = AppUrl::parse(input)?;
        let resource = match self.resources.load(&url) {
            Ok(resource) => resource,
            Err(NavigationError::ResourceNotFound(path)) => {
                self.install_error_page(&url, 404, &format!("resource not found: {path}"));
                self.record_navigation(url);
                return Ok(NavigationStatus::ErrorPage { status: 404 });
            }
            Err(error) => return Err(error),
        };

        if !is_html(&resource) {
            self.install_error_page(
                &url,
                415,
                &format!("unsupported top-level resource: {}", resource.mime_type),
            );
            self.record_navigation(url);
            return Ok(NavigationStatus::ErrorPage { status: 415 });
        }

        let source = match String::from_utf8(resource.body) {
            Ok(source) => source,
            Err(error) => {
                self.install_error_page(&url, 500, &format!("HTML resource is not UTF-8: {error}"));
                self.record_navigation(url);
                return Ok(NavigationStatus::ErrorPage { status: 500 });
            }
        };
        let document = match Document::parse(&source) {
            Ok(document) => Rc::new(RefCell::new(document)),
            Err(error) => {
                self.install_error_page(&url, 500, &error.to_string());
                self.record_navigation(url);
                return Ok(NavigationStatus::ErrorPage { status: 500 });
            }
        };
        let stylesheet_sources = document.borrow().stylesheet_sources();
        let stylesheet = match self.load_stylesheets(&url, &stylesheet_sources) {
            Ok(stylesheet) => stylesheet,
            Err(error) => {
                self.install_error_page(&url, 500, &error.to_string());
                self.record_navigation(url);
                return Ok(NavigationStatus::ErrorPage { status: 500 });
            }
        };
        let script_sources = document.borrow().script_sources();
        let mut runtime = ScriptRuntime::new(document.clone());
        if let Err(error) = self.execute_scripts(&url, &script_sources, &mut runtime) {
            self.install_error_page(&url, 500, &error.to_string());
            self.record_navigation(url);
            return Ok(NavigationStatus::ErrorPage { status: 500 });
        }

        self.page = Some(Page {
            document,
            runtime,
            stylesheet,
        });
        self.record_navigation(url);
        Ok(NavigationStatus::Loaded)
    }

    pub fn current_url(&self) -> Option<&AppUrl> {
        self.current_url.as_ref()
    }

    pub fn history(&self) -> &[AppUrl] {
        &self.history
    }

    pub fn document(&self) -> Result<Rc<RefCell<Document>>, NavigationError> {
        self.page
            .as_ref()
            .map(|page| page.document.clone())
            .ok_or(NavigationError::NoPage)
    }

    pub fn click_id(&mut self, id: &str) -> Result<bool, NavigationError> {
        self.page
            .as_mut()
            .ok_or(NavigationError::NoPage)?
            .runtime
            .click_id(id)
            .map_err(|error| NavigationError::Script(error.to_string()))
    }

    pub fn click_at(&mut self, viewport: Size, x: f32, y: f32) -> Result<bool, NavigationError> {
        self.dispatch_at(viewport, x, y, "click")
    }

    pub fn dispatch_at(
        &mut self,
        viewport: Size,
        x: f32,
        y: f32,
        event: &str,
    ) -> Result<bool, NavigationError> {
        let path = self.layout(viewport)?.hit_test(x, y);
        let Some(path) = path else {
            return Ok(false);
        };
        self.page
            .as_mut()
            .ok_or(NavigationError::NoPage)?
            .runtime
            .dispatch_path_event(path, event)
            .map_err(|error| NavigationError::Script(error.to_string()))
    }

    pub fn advance_time(&mut self, milliseconds: u64) -> Result<usize, NavigationError> {
        self.page
            .as_mut()
            .ok_or(NavigationError::NoPage)?
            .runtime
            .advance_time(milliseconds)
            .map_err(|error| NavigationError::Script(error.to_string()))
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
        Ok(build_layout(&document, &stylesheet, viewport))
    }

    pub fn paint(&self, viewport: Size) -> Result<Canvas, NavigationError> {
        Ok(self.layout(viewport)?.paint())
    }

    pub fn describe(&self, viewport: Size) -> Result<String, NavigationError> {
        Ok(self.layout(viewport)?.describe())
    }

    fn record_navigation(&mut self, url: AppUrl) {
        self.current_url = Some(url.clone());
        self.history.push(url);
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
                    let resource = self.resources.load_subresource(&url)?;
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
        for source in sources {
            let source = match source {
                ScriptSource::Inline(source) => source.clone(),
                ScriptSource::External(reference) => {
                    let url = base.resolve(reference)?;
                    let resource = self.resources.load_subresource(&url)?;
                    if !is_javascript(&resource) {
                        return Err(NavigationError::UnsupportedMimeType {
                            path: url.path().to_string(),
                            mime_type: resource.mime_type,
                        });
                    }
                    String::from_utf8(resource.body)
                        .map_err(|_| NavigationError::InvalidUtf8(url.path().to_string()))?
                }
            };
            runtime
                .execute(&source)
                .map_err(|error| NavigationError::Script(error.to_string()))?;
        }
        Ok(())
    }

    fn install_error_page(&mut self, url: &AppUrl, status: u16, detail: &str) {
        let source = error_page_source(status, &url.to_string(), detail);
        let document = Rc::new(RefCell::new(
            Document::parse(&source).expect("built-in recovery document should parse"),
        ));
        let stylesheet = document.borrow().style_text();
        self.page = Some(Page {
            runtime: ScriptRuntime::new(document.clone()),
            document,
            stylesheet,
        });
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

fn is_javascript(resource: &Resource) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
