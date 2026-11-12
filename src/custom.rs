use crate::assets::{AssetOrigin, AssetRoute, EmbeddedAssets, RequestTarget};
use crate::audio::DesktopAudio;
use crate::autostart::Autostart;
use crate::commands::{CommandRoutes, Commands};
use crate::desktop::{DesktopClipboard, DesktopFilePicker, DesktopTimeZone, user_locale};
use crate::dev::DevServer;
use crate::files::FileScope;
use crate::fonts::SystemFonts;
use crate::instance;
use crate::launch::{self, ASSETS_SWITCH, ENTRY_SWITCH};
use crate::notification::Notifier;
use crate::shortcut::{GlobalShortcuts, ShortcutState};
use crate::tray::{Tray, TrayConfig, TrayEvent};
use crate::video::DesktopVideo;
use ceri_engine::{
    APP_ORIGIN, AppUrl, Browser, Capabilities, CommandCall, CommandError, EngineHost,
    ExternalRequest, FontSet, Grant, HostBridge, HostRuntimeInfo, Lifecycle, NavigationError,
    Resource, ResourceBundle, ResourceProvider, ResourceResponse, Size, SocketConnector,
    WindowConfig, WindowControl, mime_type_for, run_window_with,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const SCREENSHOT_SWITCH: &str = "ceri-screenshot";
const SCALE_SWITCH: &str = "ceri-scale";
const ALLOW_ORIGIN_SWITCH: &str = "ceri-allow-origin";

// reaches the running app's windows from commands, hooks and threads; quiet until the loop runs
#[derive(Clone, Default)]
pub struct AppHandle {
    control: Arc<OnceLock<WindowControl>>,
    files: FileScope,
    tray: Arc<OnceLock<Tray>>,
    notifier: Arc<OnceLock<Notifier>>,
    shortcuts: GlobalShortcuts,
}

impl AppHandle {
    fn attach(&self, control: &WindowControl) {
        let _ = self.control.set(control.clone());
    }

    // opens, acts on, reads and closes windows once the loop runs
    pub fn windows(&self) -> Option<&WindowControl> {
        self.control.get()
    }

    // the folders pages may load files from, allowed and revoked at any time
    pub fn files(&self) -> &FileScope {
        &self.files
    }

    // the notification area icon the app asked for, there from before the loop runs
    pub fn tray(&self) -> Option<&Tray> {
        self.tray.get()
    }

    // shortcuts that reach the app from anywhere on the desktop, reported to
    // App::on_global_shortcut and as the global-shortcut event
    pub fn global_shortcuts(&self) -> &GlobalShortcuts {
        &self.shortcuts
    }

    // an event for every window's listeners
    pub fn emit(&self, event: &str, payload: impl Serialize) {
        self.send(None, event, payload);
    }

    pub fn emit_to(&self, window: &str, event: &str, payload: impl Serialize) {
        self.send(Some(window), event, payload);
    }

    // a desktop notification under the app's name and icon, once the app runs
    pub fn notify(&self, title: &str, body: &str) -> Result<(), String> {
        match self.notifier.get() {
            Some(notifier) => notifier.show(title, body),
            None => Err("notifications start with the app".to_string()),
        }
    }

    // closes every window without asking and ends the loop, once it runs
    pub fn exit(&self) {
        if let Some(control) = self.windows() {
            control.exit();
        }
    }

    fn send(&self, target: Option<&str>, event: &str, payload: impl Serialize) {
        let Some(control) = self.windows() else {
            return;
        };
        match serde_json::to_string(&payload) {
            Ok(payload) => control.emit(target, event, &payload),
            Err(error) => eprintln!("ceri: the {event} event is not JSON: {error}"),
        }
    }
}

// an app: its windows, its commands and the capabilities that say which pages may invoke them
#[derive(Default)]
pub struct App {
    commands: Commands,
    capabilities: Capabilities,
    windows: Vec<WindowConfig>,
    identifier: Option<String>,
    name: Option<String>,
    autostart_arguments: Vec<OsString>,
    version: Option<String>,
    lifecycle: Lifecycle,
    handle: AppHandle,
    tray: Option<TrayRequest>,
    single_instance: Option<LaunchRequest>,
    global_shortcut: Option<ShortcutHook>,
    assets: Option<EmbeddedAssets>,
    icon: Option<Vec<u8>>,
    content_security_policy: Option<String>,
    local_storage_import: Option<(PathBuf, String)>,
    dev_url: Option<String>,
}

type LaunchRequest = Box<dyn Fn(&AppHandle, Vec<OsString>, PathBuf) + Send + Sync>;

type ShortcutHook = Box<dyn Fn(&AppHandle, &str, ShortcutState) + Send + Sync>;

type TrayRequest = (TrayConfig, Box<dyn Fn(&AppHandle, TrayEvent) + Send + Sync>);

impl App {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn handle(&self) -> AppHandle {
        self.handle.clone()
    }

    // a window to open at launch; an app that names none opens one main window, and a start
    // URL that is a path is a path under the app origin
    pub fn window(mut self, config: WindowConfig) -> Self {
        self.windows.push(config);
        self
    }

    // names the app's own profile folder, as a reverse domain such as com.example.notes
    pub fn identifier(mut self, identifier: impl Into<String>) -> Self {
        self.identifier = Some(identifier.into());
        self
    }

    // the name people see the app by, on notifications; the first window's title otherwise
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    // what the app starts with when a page turns on starting at login
    pub fn autostart_arguments(
        mut self,
        arguments: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        self.autostart_arguments = arguments.into_iter().map(Into::into).collect();
        self
    }

    // a PNG or SVG for every window and the tray, in place of the pages' favicon
    pub fn icon(mut self, image: impl Into<Vec<u8>>) -> Self {
        self.icon = Some(image.into());
        self
    }

    // the content security policy every page of the app enforces, as a response header would
    // carry it; the inline scripts and styles the app ships count as its own code
    pub fn content_security_policy(mut self, policy: impl Into<String>) -> Self {
        self.content_security_policy = Some(policy.into());
        self
    }

    // the localStorage an Electron or WebView2 build of the app kept for origin, read from that
    // profile's `Local Storage/leveldb` folder on the first run that has no storage of its own
    pub fn import_local_storage(
        mut self,
        directory: impl Into<PathBuf>,
        origin: impl Into<String>,
    ) -> Self {
        self.local_storage_import = Some((directory.into(), origin.into()));
        self
    }

    // a debug build serves the app origin from this dev server, such as vite's
    // http://localhost:5173, instead of the packaged pages; a release build ignores it
    pub fn dev_url(mut self, url: impl Into<String>) -> Self {
        self.dev_url = Some(url.into());
        self
    }

    // what app:version answers pages with
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    // an icon in the notification area with a native menu; events arrive on the loop's thread
    // on Windows and on a tray thread elsewhere
    pub fn tray(
        mut self,
        config: TrayConfig,
        on_event: impl Fn(&AppHandle, TrayEvent) + Send + Sync + 'static,
    ) -> Self {
        self.tray = Some((config, Box::new(on_event)));
        self
    }

    // a later launch hands its arguments and working directory to this copy and exits; they
    // arrive on their own thread, perhaps before the windows open
    pub fn single_instance(
        mut self,
        on_launch: impl Fn(&AppHandle, Vec<OsString>, PathBuf) + Send + Sync + 'static,
    ) -> Self {
        self.single_instance = Some(Box::new(on_launch));
        self
    }

    // stays running with every window closed, until the app exits
    // each press and release of a registered global shortcut, by its canonical name
    pub fn on_global_shortcut(
        mut self,
        hook: impl Fn(&AppHandle, &str, ShortcutState) + Send + Sync + 'static,
    ) -> Self {
        self.global_shortcut = Some(Box::new(hook));
        self
    }

    pub fn keep_running(mut self) -> Self {
        self.lifecycle.keep_running = true;
        self
    }

    // runs on the loop's thread once the launch windows are open
    pub fn on_start(mut self, hook: impl FnOnce(&AppHandle) + 'static) -> Self {
        let handle = self.handle.clone();
        self.lifecycle.started = Some(Box::new(move |_| hook(&handle)));
        self
    }

    // answers whether a window may close when its user or the app asks
    pub fn on_close_requested(
        mut self,
        mut hook: impl FnMut(&AppHandle, &str) -> bool + 'static,
    ) -> Self {
        let handle = self.handle.clone();
        self.lifecycle.close_requested = Some(Box::new(move |_, label| hook(&handle, label)));
        self
    }

    pub fn on_closed(mut self, mut hook: impl FnMut(&AppHandle, &str) + 'static) -> Self {
        let handle = self.handle.clone();
        self.lifecycle.closed = Some(Box::new(move |_, label| hook(&handle, label)));
        self
    }

    // after the last window has gone, before run returns
    pub fn on_exit(mut self, hook: impl FnOnce() + 'static) -> Self {
        self.lifecycle.exiting = Some(Box::new(hook));
        self
    }

    // a command quick enough to answer on the engine thread, its arguments and result in
    // their serde shapes
    pub fn command<A, R, F>(self, name: impl Into<String>, handler: F) -> Self
    where
        A: DeserializeOwned + 'static,
        R: Serialize + 'static,
        F: Fn(A) -> Result<R, CommandError> + Send + Sync + 'static,
    {
        self.json_command(name, typed(handler))
    }

    // a command that may block, answered from a thread of its own
    pub fn blocking_command<A, R, F>(self, name: impl Into<String>, handler: F) -> Self
    where
        A: DeserializeOwned + 'static,
        R: Serialize + 'static,
        F: Fn(A) -> Result<R, CommandError> + Send + Sync + 'static,
    {
        self.blocking_json_command(name, typed(handler))
    }

    // a command that reads the call itself and answers with JSON text
    pub fn json_command<F>(mut self, name: impl Into<String>, handler: F) -> Self
    where
        F: Fn(&CommandCall) -> Result<String, CommandError> + Send + Sync + 'static,
    {
        self.commands.add(name, handler);
        self
    }

    pub fn blocking_json_command<F>(mut self, name: impl Into<String>, handler: F) -> Self
    where
        F: Fn(&CommandCall) -> Result<String, CommandError> + Send + Sync + 'static,
    {
        self.commands.add_blocking(name, handler);
        self
    }

    // the pages the app serves, compiled in with ceri::embedded_assets!() rather than read
    // from an assets folder beside the executable
    pub fn assets(mut self, files: EmbeddedAssets) -> Self {
        self.assets = Some(files);
        self
    }

    // the app origin may always drive its own window and read the app version
    pub fn capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn run(mut self) -> Result<(), String> {
        let identifier = self.identifier.take().unwrap_or_else(|| "Ceri".to_string());
        check_identifier(&identifier)?;
        if let Some((directory, origin)) = self.local_storage_import.take() {
            let storage = launch::profile_path(&identifier)?.join("storage");
            let seeded = ceri_engine::seed_local_storage(&storage, || {
                crate::chromium::local_storage(&directory, &origin)
            });
            if let Err(error) = seeded {
                eprintln!(
                    "ceri: could not bring over the localStorage in {}: {error}",
                    directory.display()
                );
            }
        }
        let dev = match self.dev_url.take().filter(|_| cfg!(debug_assertions)) {
            Some(url) => Some(DevServer::parse(&url)?),
            None => None,
        };
        let events = self.handle.clone();
        let hook = self.global_shortcut.take();
        self.handle
            .shortcuts
            .listen(Box::new(move |shortcut, state| {
                if let Some(hook) = &hook {
                    hook(&events, shortcut, state);
                }
                events.emit(
                    "global-shortcut",
                    serde_json::json!({ "shortcut": shortcut, "state": state }),
                );
            }));
        let handle = self.handle.clone();
        let started = self.lifecycle.started.take();
        self.lifecycle.started = Some(Box::new(move |control| {
            handle.attach(control);
            if let Some(started) = started {
                started(control);
            }
        }));
        let capabilities = self.capabilities.grant(Grant {
            windows: vec!["*".to_string()],
            origins: vec![APP_ORIGIN.to_string()],
            commands: vec![
                "window:*".to_string(),
                "app:*".to_string(),
                "dialog:*".to_string(),
                "opener:open-url".to_string(),
                "opener:reveal".to_string(),
                "notification:*".to_string(),
                "autostart:*".to_string(),
            ],
        });
        let name = self
            .name
            .or_else(|| self.windows.first().map(|window| window.title.clone()))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| identifier.clone());
        let autostart = Autostart::new(&identifier, &name).arguments(self.autostart_arguments);
        run_app(
            Bridge {
                commands: Arc::new(self.commands),
                capabilities,
                autostart,
                shortcuts: self.handle.shortcuts.clone(),
                name,
                version: self.version,
                files: self.handle.files.clone(),
                notifier: self.handle.notifier.clone(),
                embedded: self.assets,
                icon: self.icon,
                content_security_policy: self.content_security_policy,
                dev,
            },
            &identifier,
            self.windows,
            self.lifecycle,
            self.handle,
            self.tray,
            self.single_instance,
        )
    }
}

// the identifier becomes a folder name, so it keeps to characters every file system takes
fn check_identifier(identifier: &str) -> Result<(), String> {
    let usable = !identifier.is_empty()
        && identifier.len() <= 128
        && !identifier.starts_with('.')
        && !identifier.ends_with('.')
        && identifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    match usable {
        true => Ok(()),
        false => Err(format!(
            "the app identifier {identifier:?} must be letters, digits, dots, hyphens and underscores"
        )),
    }
}

// a handler over serde types as one over the call's JSON text
fn typed<A, R, F>(handler: F) -> impl Fn(&CommandCall) -> Result<String, CommandError>
where
    A: DeserializeOwned,
    R: Serialize,
    F: Fn(A) -> Result<R, CommandError>,
{
    move |call| {
        let arguments = serde_json::from_str(&call.payload)
            .map_err(|error| CommandError::new("invalid-payload", error.to_string()))?;
        let result = handler(arguments)?;
        serde_json::to_string(&result).map_err(|error| CommandError::failed(error.to_string()))
    }
}

// what each browser the app builds hands its pages
#[derive(Clone)]
struct Bridge {
    commands: Arc<Commands>,
    capabilities: Capabilities,
    name: String,
    version: Option<String>,
    files: FileScope,
    notifier: Arc<OnceLock<Notifier>>,
    autostart: Autostart,
    shortcuts: GlobalShortcuts,
    // the pages compiled into the app, served unless a folder is named at launch
    embedded: Option<EmbeddedAssets>,
    icon: Option<Vec<u8>>,
    content_security_policy: Option<String>,
    dev: Option<DevServer>,
}

impl Bridge {
    fn host_bridge(&self, window: &str, control: Option<&WindowControl>) -> HostBridge {
        HostBridge::new(
            Rc::new(CommandRoutes {
                commands: self.commands.clone(),
                control: control.cloned(),
                version: self.version.clone(),
                notifier: self.notifier.clone(),
                autostart: self.autostart.clone(),
                shortcuts: self.shortcuts.clone(),
            }),
            self.capabilities.clone(),
            window,
        )
    }
}

pub fn run() -> Result<(), String> {
    App::new().version(env!("CARGO_PKG_VERSION")).run()
}

fn run_app(
    bridge: Bridge,
    identifier: &str,
    windows: Vec<WindowConfig>,
    mut lifecycle: Lifecycle,
    handle: AppHandle,
    tray: Option<TrayRequest>,
    single_instance: Option<LaunchRequest>,
) -> Result<(), String> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let launch = launch::resolve(
        launch::switch_value(arguments.clone(), ASSETS_SWITCH).map(PathBuf::from),
        bridge.embedded,
        launch::switch_value(arguments.clone(), ENTRY_SWITCH),
    )?;
    let profile = launch::profile_path(identifier)?;
    let storage = profile.join("storage");
    let allowed = allowed_origins(&arguments)?;
    let assets = launch.assets.clone();
    let icon_source = bridge
        .icon
        .clone()
        .or_else(|| app_icon_source(launch.assets.as_ref()));
    let _ = bridge.notifier.set(Notifier::new(
        identifier,
        &bridge.name,
        icon_source.clone(),
        &profile,
    ));
    let icon = icon_source
        .as_deref()
        .and_then(|source| ceri_engine::rasterize_icon(source, 64));
    let windows = match windows.is_empty() && !lifecycle.keep_running {
        true => vec![WindowConfig {
            title: "Ceri".to_string(),
            start_url: launch.initial_url.clone(),
            geometry_path: Some(profile.join("window")),
            ..WindowConfig::default()
        }],
        false => windows,
    };
    let prepare = move |config: WindowConfig| WindowConfig {
        start_url: app_url(&config.start_url),
        icon: config.icon.or_else(|| icon.clone()),
        ..config
    };
    if let Some(path) = launch::switch_value(arguments.clone(), SCREENSHOT_SWITCH) {
        let config = prepare(windows.first().cloned().unwrap_or_else(|| WindowConfig {
            start_url: launch.initial_url.clone(),
            ..WindowConfig::default()
        }));
        let scale = match launch::switch_value(arguments.clone(), SCALE_SWITCH) {
            Some(value) => value
                .parse::<f32>()
                .ok()
                .filter(|scale| scale.is_finite() && *scale > 0.0)
                .ok_or_else(|| format!("--{SCALE_SWITCH} needs a positive number, not {value}"))?,
            None => 1.0,
        };
        // the engine needs a worker's stack, more than the main thread has on windows
        return std::thread::Builder::new()
            .name("ceri-engine".to_string())
            .spawn(move || {
                capture(
                    &config,
                    scale,
                    PathBuf::from(path),
                    assets,
                    storage,
                    allowed,
                    bridge,
                )
            })
            .map_err(|error| format!("could not start the capture: {error}"))?
            .join()
            .map_err(|_| "the capture stopped with a panic".to_string())?;
    }
    let _instance = match single_instance {
        Some(on_launch) => {
            let launches = handle.clone();
            // a launch that lands before the loop runs waits for it, so the app can reach its
            // windows
            let on_launch: instance::LaunchHandler = Box::new(move |arguments, directory| {
                launches.control.wait();
                on_launch(&launches, arguments, directory)
            });
            match instance::claim(&profile, identifier, &arguments, on_launch)? {
                Some(instance) => Some(instance),
                None => return Ok(()),
            }
        }
        None => None,
    };
    if let Some((config, on_event)) = tray {
        let events = handle.clone();
        let config = TrayConfig {
            icon: config.icon.or(icon_source),
            ..config
        };
        let tray = Tray::create(
            identifier,
            config,
            Box::new(move |event| on_event(&events, event)),
        );
        let _ = handle.tray.set(tray);
    }
    lifecycle.prepare = Some(Box::new(prepare));
    let tray = handle.tray.clone();
    let result = run_window_with(
        windows,
        move |window| {
            handle.attach(&window.control);
            build_browser(
                assets.clone(),
                storage.clone(),
                allowed.clone(),
                bridge.files.clone(),
                bridge.host_bridge(&window.label, Some(&window.control)),
                bridge.content_security_policy.clone(),
                bridge.dev.clone(),
            )
        },
        lifecycle,
    );
    if let Some(tray) = tray.get() {
        tray.remove();
    }
    result
}

// a start URL that is a path means one under the app origin
fn app_url(url: &str) -> String {
    match url.starts_with('/') {
        true => format!("{APP_ORIGIN}{url}"),
        false => url.to_string(),
    }
}

// renders one frame without opening a window, for smoke checks and release shots
fn capture(
    config: &WindowConfig,
    scale: f32,
    output: PathBuf,
    assets: Arc<AssetOrigin>,
    storage: PathBuf,
    allowed: Vec<String>,
    bridge: Bridge,
) -> Result<(), String> {
    let mut host = EngineHost::with_scale(
        build_browser(
            assets,
            storage,
            allowed,
            bridge.files.clone(),
            bridge.host_bridge(&config.label, None),
            bridge.content_security_policy.clone(),
            bridge.dev.clone(),
        )?,
        Size {
            width: config.width as f32,
            height: config.height as f32,
        },
        scale,
    );
    let status = host
        .navigate(&config.start_url)
        .map_err(|error| error.to_string())?;
    // a second of page time lets timers and animation frames settle the first view
    host.browser_mut()
        .advance_time(1000)
        .map_err(|error| error.to_string())?;
    let frame = host
        .render()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the engine produced no frame".to_string())?;
    frame
        .canvas
        .write_png(&output)
        .map_err(|error| format!("could not write {}: {error}", output.display()))?;
    println!(
        "{} {status}, painted {}x{} to {}",
        config.start_url,
        frame.canvas.width(),
        frame.canvas.height(),
        output.display()
    );
    for log in host.browser_mut().console_logs() {
        println!("console: {log}");
    }
    Ok(())
}

fn build_browser(
    assets: Arc<AssetOrigin>,
    storage: PathBuf,
    allowed_origins: Vec<String>,
    files: FileScope,
    bridge: HostBridge,
    content_security_policy: Option<String>,
    dev: Option<DevServer>,
) -> Result<Browser, String> {
    fs::create_dir_all(&storage)
        .map_err(|error| format!("could not create {}: {error}", storage.display()))?;
    let browser = Browser::with_resource_provider(
        ResourceBundle::default(),
        Rc::new(PackagedAssets {
            assets,
            allowed_origins,
            files,
            dev,
        }),
    )
    .with_storage_directory(&storage)
    .map_err(|error| {
        format!(
            "could not open the app storage bucket {}: {error}",
            storage.display()
        )
    })?
    .with_host_runtime_info(runtime_info())
    .with_host_bridge(bridge)
    .with_fonts(Rc::new(system_font_set()))
    .with_clipboard_provider(Rc::new(DesktopClipboard::default()))
    .with_audio_device(Rc::new(DesktopAudio))
    .with_media_decoder(Arc::new(DesktopVideo))
    .with_time_zone(Rc::new(DesktopTimeZone))
    .with_file_picker(Rc::new(DesktopFilePicker));
    let browser = match content_security_policy {
        Some(policy) => browser.with_content_security_policy(policy),
        None => browser,
    };
    Ok(match user_locale() {
        Some(locale) => browser.with_locale(locale),
        None => browser,
    })
}

// the bytes of the first packaged favicon that decodes, for the window, taskbar and tray icons
fn app_icon_source(assets: &AssetOrigin) -> Option<Vec<u8>> {
    for candidate in [
        "/web/favicon.svg",
        "/web/favicon.png",
        "/favicon.svg",
        "/favicon.png",
    ] {
        let route = assets
            .route(
                &format!("{}{candidate}", ceri_engine::APP_ORIGIN),
                RequestTarget::Subresource,
            )
            .ok()?;
        let AssetRoute::File(asset) = route else {
            continue;
        };
        let source = asset.read().ok()?;
        if ceri_engine::rasterize_icon(&source, 16).is_some() {
            return Some(source);
        }
    }
    None
}

fn system_font_set() -> FontSet {
    let fonts = SystemFonts::discover();
    if fonts.families() == 0 {
        eprintln!("ceri: no installed fonts were found, falling back to the builtin face");
        return FontSet::builtin();
    }
    FontSet::from_source(Rc::new(fonts))
}

fn runtime_info() -> HostRuntimeInfo {
    HostRuntimeInfo::new(
        format!("Ceri {}", env!("CARGO_PKG_VERSION")),
        "ready",
        session_id(),
    )
}

fn session_id() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    format!("{seconds:x}-{:x}", std::process::id())
}

// the app may only reach origins the operator listed on the command line
fn allowed_origins(arguments: &[std::ffi::OsString]) -> Result<Vec<String>, String> {
    let mut origins = Vec::new();
    let mut remaining: Vec<std::ffi::OsString> = arguments.to_vec();
    while let Some(value) = launch::switch_value(remaining.clone(), ALLOW_ORIGIN_SWITCH) {
        let origin = ceri_engine::external_url(&value)
            .map_err(|error| format!("--{ALLOW_ORIGIN_SWITCH} {value} is not usable: {error}"))?;
        let origin = origin.trim_end_matches('/').to_string();
        if !origins.contains(&origin) {
            origins.push(origin);
        }
        let flag = format!("--{ALLOW_ORIGIN_SWITCH}");
        let inline = format!("{flag}={value}");
        let position = remaining.iter().position(|argument| {
            let argument = argument.to_string_lossy();
            argument == inline || argument == flag
        });
        match position {
            Some(index) => {
                let inline_form = remaining[index].to_string_lossy().contains('=');
                remaining.remove(index);
                if !inline_form && index < remaining.len() {
                    remaining.remove(index);
                }
            }
            None => break,
        }
    }
    Ok(origins)
}

// serves the packaged app through the asset origin policy
struct PackagedAssets {
    assets: Arc<AssetOrigin>,
    allowed_origins: Vec<String>,
    files: FileScope,
    dev: Option<DevServer>,
}

impl ResourceProvider for PackagedAssets {
    fn provide(&self, url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
        if url.is_files() {
            let Some(path) = self.files.resolve(url.path()) else {
                return Ok(None);
            };
            return read_resource(&path).map(Some);
        }
        if let Some(dev) = &self.dev {
            return dev.get(url);
        }
        let request = url.to_string();
        let target = if is_document_path(url.path()) {
            RequestTarget::TopLevelNavigation
        } else {
            RequestTarget::Subresource
        };
        let route = self
            .assets
            .route(&request, target)
            .map_err(|error| NavigationError::ResourceProvider(error.to_string()))?;
        let asset = match route {
            AssetRoute::File(asset) | AssetRoute::AppShell(asset) => asset,
            AssetRoute::NotFound => return Ok(None),
            AssetRoute::Rejected(reason) => {
                return Err(NavigationError::ResourceProvider(format!(
                    "{} was rejected: {reason:?}",
                    url.path()
                )));
            }
        };
        let body = asset.read().map_err(|error| {
            NavigationError::ResourceProvider(format!("could not read {}: {error}", url.path()))
        })?;
        Ok(Some(Resource::new(mime_type_for(&asset.path), body)))
    }

    fn provide_external(
        &self,
        request: &ExternalRequest,
    ) -> Result<Option<ResourceResponse>, NavigationError> {
        if !self
            .allowed_origins
            .iter()
            .any(|origin| origin.eq_ignore_ascii_case(request.origin()))
        {
            return Ok(None);
        }
        crate::http::request(request).map(Some)
    }

    fn media_file(&self, url: &AppUrl) -> Option<PathBuf> {
        url.is_files().then(|| self.files.resolve(url.path()))?
    }

    fn connect_socket(&self, url: &str) -> Option<SocketConnector> {
        let rest = url
            .strip_prefix("wss://")
            .or_else(|| url.strip_prefix("ws://"))?;
        let host = rest.split(['/', '?']).next()?;
        let app = APP_ORIGIN.strip_prefix("https://")?;
        self.dev
            .as_ref()
            .filter(|_| host == app)
            .map(DevServer::socket)
    }
}

fn read_resource(path: &Path) -> Result<Resource, NavigationError> {
    let body = fs::read(path).map_err(|error| {
        NavigationError::ResourceProvider(format!("could not read {}: {error}", path.display()))
    })?;
    Ok(Resource::new(mime_type_for(path), body))
}

fn is_document_path(path: &str) -> bool {
    let last = path.rsplit('/').next().unwrap_or_default();
    !last.contains('.') || last.ends_with(".html") || last.ends_with(".htm")
}

#[cfg(test)]
mod tests {
    use super::{
        CommandCall, PackagedAssets, allowed_origins, check_identifier, is_document_path, typed,
    };
    use crate::assets::AssetOrigin;
    use ceri_engine::{AppUrl, ExternalRequest, ResourceProvider};
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::sync::Arc;

    #[test]
    fn document_paths_use_the_navigation_route() {
        assert!(is_document_path("/"));
        assert!(is_document_path("/settings"));
        assert!(is_document_path("/index.html"));
        assert!(!is_document_path("/main.js"));
        assert!(!is_document_path("/styles.css"));
    }

    fn files_url(path: &std::path::Path) -> AppUrl {
        let path = path.to_str().expect("a unicode path").replace('\\', "/");
        let path = path.replacen(':', "", 1);
        AppUrl::parse(&format!(
            "https://files.ceri.invalid/{}",
            path.trim_start_matches('/')
        ))
        .expect("url")
    }

    #[test]
    fn serves_packaged_files_and_reports_missing_ones() {
        let root = std::env::current_dir()
            .expect("working directory should resolve")
            .join("web");
        let provider = PackagedAssets {
            assets: Arc::new(AssetOrigin::new(&root).expect("web root should exist")),
            allowed_origins: Vec::new(),
            files: Default::default(),
            dev: None,
        };

        let index = provider
            .provide(&AppUrl::parse("https://app.ceri.invalid/index.html").expect("url"))
            .expect("index should resolve")
            .expect("index should exist");
        assert_eq!(index.mime_type(), "text/html");
        assert!(!index.body().is_empty());

        assert!(
            provider
                .provide(&AppUrl::parse("https://app.ceri.invalid/missing.js").expect("url"))
                .expect("missing subresources should not fail")
                .is_none()
        );

        let granted = std::env::temp_dir().join(format!("ceri-provider-{}", std::process::id()));
        std::fs::create_dir_all(&granted).expect("grant folder");
        std::fs::write(granted.join("frame.png"), b"png").expect("granted file");
        provider.files.allow(&granted).expect("allow");
        let shot = provider
            .provide(&files_url(&granted.join("frame.png")))
            .expect("a granted file should resolve")
            .expect("a granted file should exist");
        assert_eq!(shot.mime_type(), "image/png");
        assert_eq!(shot.body(), b"png");
        assert!(
            provider
                .provide(&files_url(&granted.join("none.png")))
                .expect("a missing granted file should not fail")
                .is_none()
        );
        std::fs::remove_dir_all(&granted).expect("cleanup");
    }

    #[test]
    fn external_fetches_need_an_allowed_origin() {
        let root = std::env::current_dir()
            .expect("working directory should resolve")
            .join("web");
        let provider = PackagedAssets {
            assets: Arc::new(AssetOrigin::new(&root).expect("web root should exist")),
            allowed_origins: vec!["https://api.example.test".to_string()],
            files: Default::default(),
            dev: None,
        };
        let refused = ExternalRequest::new(
            "https://other.example.test/data",
            "GET",
            BTreeMap::new(),
            Vec::new(),
        )
        .expect("request should build");
        assert!(
            provider
                .provide_external(&refused)
                .expect("a refusal is not an error")
                .is_none()
        );
        let allowed = ExternalRequest::new(
            "https://api.example.test/data",
            "GET",
            BTreeMap::new(),
            Vec::new(),
        )
        .expect("request should build");
        assert_eq!(allowed.origin(), "https://api.example.test");
    }

    #[test]
    fn reads_repeated_allow_origin_switches() {
        let arguments: Vec<OsString> = [
            "--ceri-allow-origin=https://api.example.test",
            "--ceri-allow-origin",
            "https://cdn.example.test",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        assert_eq!(
            allowed_origins(&arguments).expect("origins should parse"),
            vec![
                "https://api.example.test".to_string(),
                "https://cdn.example.test".to_string()
            ]
        );
        assert!(
            allowed_origins(&[OsString::from("--ceri-allow-origin=http://insecure.test")]).is_err()
        );
    }

    #[test]
    fn identifiers_are_folder_names() {
        for identifier in ["Ceri", "lt.rot.clip-recorder", "com.example.notes_2"] {
            assert!(check_identifier(identifier).is_ok(), "{identifier}");
        }
        for identifier in [
            "",
            "..",
            ".hidden",
            "a/b",
            "a\\b",
            "a:b",
            "notes.",
            "caf\u{e9}",
        ] {
            assert!(check_identifier(identifier).is_err(), "{identifier}");
        }
    }

    #[derive(serde::Deserialize)]
    struct Greeting {
        name: String,
    }

    #[test]
    fn typed_commands_read_and_write_serde_shapes() {
        let handler = typed(|greeting: Greeting| Ok(vec![format!("hello {}", greeting.name)]));
        let call = |payload: &str| CommandCall {
            window: "main".to_string(),
            origin: ceri_engine::APP_ORIGIN.to_string(),
            command: "greet".to_string(),
            payload: payload.to_string(),
            ..Default::default()
        };
        assert_eq!(
            handler(&call("{\"name\":\"ceri\"}")),
            Ok("[\"hello ceri\"]".to_string())
        );
        assert_eq!(
            handler(&call("{}")).map_err(|error| error.code),
            Err("invalid-payload".to_string())
        );
    }
}
