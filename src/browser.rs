use crate::app::{is_app_url, is_internal_candidate};
use crate::assets::{APP_ORIGIN, AssetOrigin, AssetRoute, RequestTarget};
use cef::*;
use std::cell::RefCell;
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[derive(Clone)]
pub struct LaunchConfig {
    pub assets: Arc<AssetOrigin>,
    pub initial_url: String,
}

pub fn create_browser_process(launch: LaunchConfig) -> BrowserProcessHandler {
    CeriBrowserProcess::new(launch, RefCell::new(None))
}

wrap_browser_process_handler! {
    struct CeriBrowserProcess {
        launch: LaunchConfig,
        client: RefCell<Option<Client>>,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);

            let state = BrowserState::new();
            let client = create_client(state, self.launch.clone());
            *self.client.borrow_mut() = Some(client);

            let mut client = self.default_client();
            let scheme = CefString::from("https");
            let domain = CefString::from("app.ceri.invalid");
            let mut scheme_factory = CeriSchemeFactory::new(
                self.launch.assets.clone(),
                self.launch.initial_url.clone(),
            );
            if register_scheme_handler_factory(
                Some(&scheme),
                Some(&domain),
                Some(&mut scheme_factory),
            ) == 0
            {
                eprintln!("ceri: could not register the app scheme handler");
            }

            let mut delegate = CeriBrowserViewDelegate::new(RuntimeStyle::ALLOY);
            let url = CefString::from(self.launch.initial_url.as_str());
            let settings = BrowserSettings {
                javascript: State::ENABLED,
                local_storage: State::ENABLED,
                webgl: State::ENABLED,
                background_color: 0xff0c0e12,
                ..Default::default()
            };
            let browser_view = browser_view_create(
                client.as_mut(),
                Some(&url),
                Some(&settings),
                None,
                None,
                Some(&mut delegate),
            );
            let mut window_delegate = CeriWindowDelegate::new(
                RefCell::new(browser_view),
            );
            window_create_top_level(Some(&mut window_delegate));
        }

        fn default_client(&self) -> Option<Client> {
            self.client.borrow().clone()
        }
    }
}

wrap_browser_view_delegate! {
    struct CeriBrowserViewDelegate {
        runtime_style: RuntimeStyle,
    }

    impl ViewDelegate {}

    impl BrowserViewDelegate {
        fn browser_runtime_style(&self) -> RuntimeStyle {
            self.runtime_style
        }
    }
}

wrap_window_delegate! {
    struct CeriWindowDelegate {
        browser_view: RefCell<Option<BrowserView>>,
    }

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            Size {
                width: 1120,
                height: 720,
            }
        }
    }

    impl PanelDelegate {}

    impl WindowDelegate {
        fn on_window_created(&self, window: Option<&mut Window>) {
            let browser_view = self.browser_view.borrow();
            let (Some(window), Some(browser_view)) = (window, browser_view.as_ref()) else {
                return;
            };
            let mut view = View::from(browser_view);
            window.add_child_view(Some(&mut view));
            window.set_title(Some(&CefString::from("Ceri")));
            window.show();
        }

        fn on_window_destroyed(&self, _window: Option<&mut Window>) {
            *self.browser_view.borrow_mut() = None;
        }

        fn can_close(&self, _window: Option<&mut Window>) -> i32 {
            let browser_view = self.browser_view.borrow();
            let Some(browser_view) = browser_view.as_ref() else {
                return 1;
            };
            browser_view
                .browser()
                .and_then(|browser| browser.host())
                .map_or(1, |host| host.try_close_browser())
        }

        fn window_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

static BROWSER_STATE: OnceLock<Weak<Mutex<BrowserState>>> = OnceLock::new();

struct BrowserState {
    browsers: Vec<Browser>,
}

impl BrowserState {
    fn new() -> Arc<Mutex<Self>> {
        let state = Arc::new(Mutex::new(Self {
            browsers: Vec::new(),
        }));
        let _ = BROWSER_STATE.set(Arc::downgrade(&state));
        state
    }

    fn after_created(&mut self, browser: Option<&mut Browser>) {
        if let Some(browser) = browser.cloned() {
            self.browsers.push(browser);
        }
    }

    fn before_close(&mut self, browser: Option<&mut Browser>) {
        let Some(mut browser) = browser.cloned() else {
            return;
        };
        if let Some(index) = self
            .browsers
            .iter()
            .position(|candidate| candidate.is_same(Some(&mut browser)) != 0)
        {
            self.browsers.remove(index);
        }
        if self.browsers.is_empty() {
            quit_message_loop();
        }
    }

    fn show(&self) {
        let Some(mut browser) = self.browsers.first().cloned() else {
            return;
        };
        if let Some(browser_view) = browser_view_get_for_browser(Some(&mut browser)) {
            if let Some(window) = browser_view.window() {
                window.show();
                window.activate();
            }
        }
    }

    fn close_all(&self) {
        for browser in &self.browsers {
            if let Some(host) = browser.host() {
                host.close_browser(0);
            }
        }
    }
}

pub(crate) fn show_main_window() {
    with_browser_state(|state| state.show());
}

pub(crate) fn close_all_browsers() {
    with_browser_state(|state| state.close_all());
}

fn with_browser_state(action: impl FnOnce(&BrowserState) + Send + 'static) {
    if currently_on(ThreadId::UI) != 0 {
        if let Some(state) = BROWSER_STATE.get().and_then(Weak::upgrade) {
            if let Ok(state) = state.lock() {
                action(&state);
            }
        }
        return;
    }

    let mut task = BrowserStateTask::new(Arc::new(Mutex::new(Some(Box::new(action)))));
    post_task(ThreadId::UI, Some(&mut task));
}

type BrowserStateAction = Box<dyn FnOnce(&BrowserState) + Send>;

wrap_task! {
    struct BrowserStateTask {
        action: Arc<Mutex<Option<BrowserStateAction>>>,
    }

    impl Task {
        fn execute(&self) {
            let action = self.action.lock().ok().and_then(|mut action| action.take());
            let state = BROWSER_STATE.get().and_then(Weak::upgrade);
            if let (Some(action), Some(state)) = (action, state) {
                if let Ok(state) = state.lock() {
                    action(&state);
                }
            }
        }
    }
}

fn create_client(state: Arc<Mutex<BrowserState>>, launch: LaunchConfig) -> Client {
    CeriClient::new(
        state,
        launch.assets,
        launch.initial_url,
        Arc::new(AtomicUsize::new(0)),
    )
}

wrap_client! {
    struct CeriClient {
        state: Arc<Mutex<BrowserState>>,
        assets: Arc<AssetOrigin>,
        initial_url: String,
        consecutive_renderer_failures: Arc<AtomicUsize>,
    }

    impl Client {
        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(CeriDisplay::new())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(CeriLifeSpan::new(self.state.clone()))
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(CeriLoad::new(
                self.initial_url.clone(),
                self.consecutive_renderer_failures.clone(),
            ))
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(CeriRequest::new(
                self.assets.clone(),
                self.initial_url.clone(),
                self.consecutive_renderer_failures.clone(),
            ))
        }
    }
}

wrap_display_handler! {
    struct CeriDisplay;

    impl DisplayHandler {
        fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>) {
            let Some(mut browser) = browser.cloned() else {
                return;
            };
            if let Some(browser_view) = browser_view_get_for_browser(Some(&mut browser)) {
                if let Some(window) = browser_view.window() {
                    window.set_title(title);
                }
            }
        }
    }
}

wrap_life_span_handler! {
    struct CeriLifeSpan {
        state: Arc<Mutex<BrowserState>>,
    }

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: i32,
            _target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: i32,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut i32>,
        ) -> i32 {
            1
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            if let Ok(mut state) = self.state.lock() {
                state.after_created(browser);
            }
        }

        fn do_close(&self, _browser: Option<&mut Browser>) -> i32 {
            0
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            if let Ok(mut state) = self.state.lock() {
                state.before_close(browser);
            }
        }
    }
}

wrap_load_handler! {
    struct CeriLoad {
        initial_url: String,
        consecutive_renderer_failures: Arc<AtomicUsize>,
    }

    impl LoadHandler {
        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            http_status_code: i32,
        ) {
            if frame.is_some_and(|frame| frame.is_main() != 0)
                && (200..400).contains(&http_status_code)
            {
                self.consecutive_renderer_failures.store(0, Ordering::Release);
            }
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            _error_text: Option<&CefString>,
            _failed_url: Option<&CefString>,
        ) {
            if error_code == Errorcode::ABORTED {
                return;
            }
            let Some(frame) = frame.filter(|frame| frame.is_main() != 0) else {
                return;
            };
            let page = recovery_document(
                "The application could not be opened",
                "Ceri kept the browser-owned failure page out of view. Check the app bundle, then try again.",
                &self.initial_url,
            );
            frame.load_url(Some(&CefString::from(data_uri(&page).as_str())));
        }
    }
}

wrap_request_handler! {
    struct CeriRequest {
        assets: Arc<AssetOrigin>,
        initial_url: String,
        consecutive_renderer_failures: Arc<AtomicUsize>,
    }

    impl RequestHandler {
        fn on_before_browse(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            request: Option<&mut Request>,
            _user_gesture: i32,
            _is_redirect: i32,
        ) -> i32 {
            let is_main = frame.as_ref().is_some_and(|frame| frame.is_main() != 0);
            if !is_main {
                return 0;
            }
            let url = request
                .as_ref()
                .map(|request| CefString::from(&request.url()).to_string())
                .unwrap_or_default();
            if is_app_url(&url) || url.starts_with("data:text/html") {
                return 0;
            }
            1
        }

        fn on_open_urlfrom_tab(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _target_url: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: i32,
        ) -> i32 {
            1
        }

        fn on_render_process_terminated(
            &self,
            browser: Option<&mut Browser>,
            _status: TerminationStatus,
            _error_code: i32,
            _error_string: Option<&CefString>,
        ) {
            let failures = self
                .consecutive_renderer_failures
                .fetch_add(1, Ordering::AcqRel);
            let Some(browser) = browser else {
                return;
            };
            if failures == 0 {
                browser.reload();
                return;
            }
            if let Some(frame) = browser.main_frame() {
                let page = recovery_document(
                    "The renderer stopped twice",
                    "Ceri stopped the restart loop. Reload when you are ready to try the application again.",
                    &self.initial_url,
                );
                frame.load_url(Some(&CefString::from(data_uri(&page).as_str())));
            }
        }
    }
}

wrap_scheme_handler_factory! {
    struct CeriSchemeFactory {
        assets: Arc<AssetOrigin>,
        initial_url: String,
    }

    impl SchemeHandlerFactory {
        fn create(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            request.and_then(|request| resource_handler_for(&self.assets, &self.initial_url, request))
        }
    }
}

fn resource_handler_for(
    assets: &AssetOrigin,
    initial_url: &str,
    request: &Request,
) -> Option<ResourceHandler> {
    let url = CefString::from(&request.url()).to_string();
    if !is_internal_candidate(&url) {
        return None;
    }
    let target = if request.resource_type() == ResourceType::MAIN_FRAME {
        RequestTarget::TopLevelNavigation
    } else {
        RequestTarget::Subresource
    };
    match assets.route(&url, target) {
        Ok(AssetRoute::File(asset) | AssetRoute::AppShell(asset)) => {
            Some(file_response(asset.path, &asset.relative_path))
        }
        Ok(AssetRoute::NotFound) => Some(error_response(
            404,
            "Application file missing",
            "The installed app does not contain the requested file.",
            initial_url,
        )),
        Ok(AssetRoute::Rejected(_)) => Some(error_response(
            400,
            "Unsafe application path blocked",
            "Ceri rejected the requested path before it reached the filesystem.",
            initial_url,
        )),
        Err(_) => Some(error_response(
            503,
            "Application files unavailable",
            "Ceri could not read the application bundle.",
            initial_url,
        )),
    }
}

enum ResponseSource {
    File(PathBuf),
    Memory(Vec<u8>),
}

struct ResponseState {
    source: Option<ResponseSource>,
    reader: Option<Box<dyn Read + Send>>,
    status: i32,
    status_text: String,
    mime_type: String,
    length: i64,
    app_document: bool,
}

wrap_resource_handler! {
    struct CeriResource {
        state: Arc<Mutex<ResponseState>>,
    }

    impl ResourceHandler {
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut i32>,
            _callback: Option<&mut Callback>,
        ) -> i32 {
            let Ok(mut state) = self.state.lock() else {
                return 0;
            };
            let Some(source) = state.source.take() else {
                return 0;
            };
            match source {
                ResponseSource::File(path) => match File::open(path) {
                    Ok(file) => state.reader = Some(Box::new(file)),
                    Err(_) => state.replace_with_failure(),
                },
                ResponseSource::Memory(bytes) => {
                    state.reader = Some(Box::new(Cursor::new(bytes)));
                }
            }
            if let Some(handle_request) = handle_request {
                *handle_request = 1;
            }
            1
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            let (Some(response), Ok(state)) = (response, self.state.lock()) else {
                return;
            };
            response.set_status(state.status);
            response.set_status_text(Some(&CefString::from(state.status_text.as_str())));
            response.set_mime_type(Some(&CefString::from(state.mime_type.as_str())));
            let mut headers = security_headers(state.app_document);
            response.set_header_map(Some(&mut headers));
            if let Some(response_length) = response_length {
                *response_length = state.length;
            }
        }

        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: i32,
            bytes_read: Option<&mut i32>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> i32 {
            if bytes_to_read <= 0 {
                return 0;
            }
            let Some(bytes_read) = bytes_read else {
                return 0;
            };
            *bytes_read = 0;
            let Ok(mut state) = self.state.lock() else {
                return 0;
            };
            let Some(reader) = state.reader.as_mut() else {
                return 0;
            };
            let output = unsafe {
                std::slice::from_raw_parts_mut(data_out, bytes_to_read as usize)
            };
            match reader.read(output) {
                Ok(0) | Err(_) => 0,
                Ok(read) => {
                    *bytes_read = read as i32;
                    1
                }
            }
        }
    }
}

impl ResponseState {
    fn replace_with_failure(&mut self) {
        let body = recovery_document(
            "Application file unavailable",
            "The file existed during startup but could not be opened now.",
            APP_ORIGIN,
        )
        .into_bytes();
        self.status = 503;
        self.status_text = "Service Unavailable".to_string();
        self.mime_type = "text/html; charset=utf-8".to_string();
        self.length = body.len() as i64;
        self.app_document = false;
        self.reader = Some(Box::new(Cursor::new(body)));
    }
}

fn file_response(path: PathBuf, relative_path: &Path) -> ResourceHandler {
    let length = std::fs::metadata(&path)
        .map(|metadata| metadata.len() as i64)
        .unwrap_or(-1);
    CeriResource::new(Arc::new(Mutex::new(ResponseState {
        source: Some(ResponseSource::File(path)),
        reader: None,
        status: 200,
        status_text: "OK".to_string(),
        mime_type: mime_type(relative_path).to_string(),
        length,
        app_document: true,
    })))
}

fn error_response(status: i32, title: &str, detail: &str, initial_url: &str) -> ResourceHandler {
    let body = recovery_document(title, detail, initial_url).into_bytes();
    let length = body.len() as i64;
    CeriResource::new(Arc::new(Mutex::new(ResponseState {
        source: Some(ResponseSource::Memory(body)),
        reader: None,
        status,
        status_text: match status {
            400 => "Bad Request",
            404 => "Not Found",
            _ => "Service Unavailable",
        }
        .to_string(),
        mime_type: "text/html; charset=utf-8".to_string(),
        length,
        app_document: false,
    })))
}

fn security_headers(app_document: bool) -> CefStringMultimap {
    let mut headers = CefStringMultimap::new();
    headers.append("Cache-Control", "no-cache");
    headers.append("Referrer-Policy", "no-referrer");
    headers.append("X-Content-Type-Options", "nosniff");
    headers.append("X-Frame-Options", "DENY");
    if app_document {
        headers.append(
            "Content-Security-Policy",
            "default-src 'self'; connect-src 'self' https: wss:; img-src 'self' data: blob:; media-src 'self' blob:; script-src 'self'; style-src 'self'; font-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        );
    } else {
        headers.append(
            "Content-Security-Policy",
            "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'",
        );
    }
    headers
}

fn mime_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html" | "htm") => "text/html",
        Some("css") => "text/css",
        Some("js" | "mjs") => "text/javascript",
        Some("json" | "map") => "application/json",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("webm") => "video/webm",
        Some("mp4") => "video/mp4",
        Some("ogg") => "audio/ogg",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("txt") => "text/plain",
        Some("xml") => "application/xml",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn data_uri(document: &str) -> String {
    let encoded = CefString::from(&base64_encode(Some(document.as_bytes())));
    let encoded = CefString::from(&uriencode(Some(&encoded), 0)).to_string();
    format!("data:text/html;base64,{encoded}")
}

fn recovery_document(title: &str, detail: &str, initial_url: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{title}</title><style>:root{{color-scheme:dark}}*{{box-sizing:border-box}}body{{margin:0;min-height:100vh;display:grid;place-items:center;background:#08131d;color:#eaf4f8;font:16px/1.5 ui-sans-serif,system-ui,sans-serif}}main{{width:min(560px,calc(100vw - 40px));border-top:3px solid #42d3b4;padding:32px 0}}p{{color:#9fb6bf}}a{{display:inline-block;margin-top:16px;color:#08131d;background:#b9f4df;padding:10px 15px;text-decoration:none;font-weight:700}}</style></head><body><main><p>Ceri recovery</p><h1>{title}</h1><p>{detail}</p><a href="{initial_url}">Try the application again</a></main></body></html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_common_web_types() {
        assert_eq!(mime_type(Path::new("app.js")), "text/javascript");
        assert_eq!(mime_type(Path::new("engine.wasm")), "application/wasm");
        assert_eq!(mime_type(Path::new("font.WOFF2")), "font/woff2");
    }

    #[test]
    fn trusted_url_requires_an_origin_boundary() {
        assert!(is_app_url("https://app.ceri.invalid/web/index.html"));
        assert!(!is_app_url("https://app.ceri.invalid.evil/web/index.html"));
    }
}
