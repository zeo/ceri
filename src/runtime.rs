use crate::app::create_app;
use crate::assets::{APP_ORIGIN, AssetOrigin, AssetRoute, RequestTarget};
use crate::browser::{LaunchConfig, close_all_browsers, show_main_window};
use crate::platform;
#[cfg(not(target_os = "windows"))]
use cef::ImplCommandLine;
use cef::{CefString, CommandLine, MainArgs, Settings, api_hash, args::Args, execute_process};
use std::error::Error;
#[cfg(target_os = "windows")]
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ENTRY_SWITCH: &str = "ceri-entry";
const ASSETS_SWITCH: &str = "ceri-assets";

#[derive(Debug)]
pub struct RuntimeError(String);

impl RuntimeError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for RuntimeError {}

#[cfg(target_os = "macos")]
type LibraryGuard = cef::library_loader::LibraryLoader;

#[cfg(not(target_os = "macos"))]
struct LibraryGuard;

pub fn run(sandbox_info: *mut u8) -> Result<(), RuntimeError> {
    let _library = load_cef(false)?;
    let args = Args::new();
    run_with_args(
        args.as_main_args(),
        command_line_for_run(&args),
        sandbox_info,
    )
}

pub fn run_helper() -> Result<i32, RuntimeError> {
    let args = Args::new();

    #[cfg(all(target_os = "macos", feature = "sandbox"))]
    let _sandbox = {
        let mut sandbox = cef::sandbox::Sandbox::new();
        sandbox.initialize(args.as_main_args());
        sandbox
    };

    let _library = load_cef(true)?;
    let mut app = create_app(None);
    Ok(execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    ))
}

pub(crate) fn run_with_args(
    main_args: &MainArgs,
    command_line: Option<CommandLine>,
    sandbox_info: *mut u8,
) -> Result<(), RuntimeError> {
    let command_line = ProcessCommandLine::from_optional(command_line)?;
    let is_browser_process = command_line.is_browser_process();

    let launch = if is_browser_process {
        Some(read_launch_config(&command_line)?)
    } else {
        None
    };
    if is_browser_process {
        platform::prepare_application().map_err(RuntimeError::new)?;
    }
    let mut app = create_app(launch.clone());
    let process_exit = execute_process(Some(main_args), Some(&mut app), sandbox_info);
    if !is_browser_process {
        if process_exit < 0 {
            return Err(RuntimeError::new("CEF rejected a subprocess launch"));
        }
        return Ok(());
    }
    if process_exit != -1 {
        return Err(RuntimeError::new(format!(
            "CEF returned unexpected browser-process code {process_exit}"
        )));
    }

    let launch = launch.expect("browser launch configuration missing");
    let profile = profile_path()?;
    std::fs::create_dir_all(&profile).map_err(|error| {
        RuntimeError::new(format!(
            "could not create browser profile {}: {error}",
            profile.display()
        ))
    })?;

    let settings = Settings {
        no_sandbox: i32::from(!cfg!(feature = "sandbox")),
        cache_path: CefString::from(profile.to_string_lossy().as_ref()),
        root_cache_path: CefString::from(profile.to_string_lossy().as_ref()),
        persist_session_cookies: 1,
        locale: CefString::from("en-US"),
        user_agent_product: CefString::from("Ceri/0.1.0"),
        background_color: 0xff0c0e12,
        remote_debugging_port: 0,
        ..Default::default()
    };

    if cef::initialize(
        Some(main_args),
        Some(&settings),
        Some(&mut app),
        sandbox_info,
    ) == 0
    {
        return Err(RuntimeError::new("CEF initialization failed"));
    }

    let _platform = platform::retain_app_delegate(show_main_window, close_all_browsers)
        .map_err(RuntimeError::new)?;
    cef::run_message_loop();
    cef::shutdown();
    drop(launch);
    Ok(())
}

fn load_cef(_helper: bool) -> Result<LibraryGuard, RuntimeError> {
    #[cfg(target_os = "macos")]
    let library = {
        let executable = std::env::current_exe()
            .map_err(|error| RuntimeError::new(format!("could not locate executable: {error}")))?;
        let loader = cef::library_loader::LibraryLoader::new(&executable, _helper);
        if !loader.load() {
            return Err(RuntimeError::new(
                "could not load the bundled CEF framework",
            ));
        }
        loader
    };

    #[cfg(not(target_os = "macos"))]
    let library = LibraryGuard;

    prepare_cef()?;
    Ok(library)
}

pub(crate) fn prepare_cef() -> Result<(), RuntimeError> {
    if api_hash(cef::sys::CEF_API_VERSION_LAST, 0).is_null() {
        return Err(RuntimeError::new("CEF API version negotiation failed"));
    }
    Ok(())
}

fn read_launch_config(command_line: &ProcessCommandLine) -> Result<LaunchConfig, RuntimeError> {
    let asset_root = command_line
        .switch_value(ASSETS_SWITCH)
        .map(PathBuf::from)
        .unwrap_or_else(default_asset_root);
    let assets = AssetOrigin::new(&asset_root).map_err(|error| {
        RuntimeError::new(format!(
            "asset root {} is unavailable: {error}",
            asset_root.display()
        ))
    })?;

    let entry_path = command_line
        .switch_value(ENTRY_SWITCH)
        .unwrap_or_else(|| detect_entry(assets.root()).to_string());
    let entry_path = normalized_entry(&entry_path)?;
    let initial_url = format!("{APP_ORIGIN}{entry_path}");
    match assets
        .route(&initial_url, RequestTarget::TopLevelNavigation)
        .map_err(|error| RuntimeError::new(format!("could not inspect app entry: {error}")))?
    {
        AssetRoute::File(_) | AssetRoute::AppShell(_) => {}
        AssetRoute::NotFound => {
            return Err(RuntimeError::new(format!(
                "app entry {entry_path} does not exist under {}",
                assets.root().display()
            )));
        }
        AssetRoute::Rejected(reason) => {
            return Err(RuntimeError::new(format!(
                "app entry {entry_path} is unsafe: {reason:?}"
            )));
        }
    }

    Ok(LaunchConfig {
        assets: Arc::new(assets),
        initial_url,
    })
}

fn command_line_for_run(args: &Args) -> Option<CommandLine> {
    #[cfg(target_os = "windows")]
    let _ = args;

    #[cfg(not(target_os = "windows"))]
    return args.as_cmd_line();

    #[cfg(target_os = "windows")]
    None
}

enum ProcessCommandLine {
    #[cfg(target_os = "windows")]
    Windows(Vec<OsString>),
    #[cfg(not(target_os = "windows"))]
    Cef(CommandLine),
}

impl ProcessCommandLine {
    fn from_optional(command_line: Option<CommandLine>) -> Result<Self, RuntimeError> {
        #[cfg(target_os = "windows")]
        {
            let _ = command_line;
            return Ok(Self::Windows(std::env::args_os().skip(1).collect()));
        }

        #[cfg(not(target_os = "windows"))]
        command_line
            .map(Self::Cef)
            .ok_or_else(|| RuntimeError::new("invalid command line"))
    }

    fn is_browser_process(&self) -> bool {
        !self.has_switch("type")
    }

    fn has_switch(&self, name: &str) -> bool {
        #[cfg(target_os = "windows")]
        {
            let exact = format!("--{name}");
            let prefix = format!("{exact}=");
            return self.args().iter().any(|argument| {
                let argument = argument.to_string_lossy();
                argument == exact || argument.starts_with(&prefix)
            });
        }

        #[cfg(not(target_os = "windows"))]
        {
            let name = CefString::from(name);
            matches!(self, Self::Cef(command_line) if command_line.has_switch(Some(&name)) != 0)
        }
    }

    fn switch_value(&self, name: &str) -> Option<String> {
        #[cfg(target_os = "windows")]
        {
            let exact = format!("--{name}");
            let prefix = format!("{exact}=");
            let args = self.args();
            for (index, argument) in args.iter().enumerate() {
                let argument = argument.to_string_lossy();
                if let Some(value) = argument.strip_prefix(&prefix) {
                    if !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
                if argument == exact {
                    let value = args.get(index + 1)?.to_string_lossy();
                    if !value.starts_with("--") && !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
            return None;
        }

        #[cfg(not(target_os = "windows"))]
        {
            let name = CefString::from(name);
            let Self::Cef(command_line) = self;
            if command_line.has_switch(Some(&name)) == 0 {
                return None;
            }
            let value = CefString::from(&command_line.switch_value(Some(&name))).to_string();
            (!value.is_empty()).then_some(value)
        }
    }

    #[cfg(target_os = "windows")]
    fn args(&self) -> &[OsString] {
        let Self::Windows(args) = self;
        args
    }
}

fn normalized_entry(entry: &str) -> Result<String, RuntimeError> {
    if entry.contains("://") || entry.contains(['?', '#', '\0']) {
        return Err(RuntimeError::new(
            "app entry must be an absolute origin path",
        ));
    }
    Ok(if entry.starts_with('/') {
        entry.to_string()
    } else {
        format!("/{entry}")
    })
}

fn detect_entry(asset_root: &Path) -> &'static str {
    if asset_root.join("index.html").is_file() {
        "/index.html"
    } else {
        "/web/index.html"
    }
}

fn default_asset_root() -> PathBuf {
    let beside_executable = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("assets")));
    if let Some(path) = beside_executable.filter(|path| path.is_dir()) {
        return path;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn profile_path() -> Result<PathBuf, RuntimeError> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);

    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library").join("Application Support"));

    #[cfg(target_os = "linux")]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".cache"))
        });

    base.map(|path| path.join("Ceri").join("profile"))
        .ok_or_else(|| RuntimeError::new("could not resolve a per-user profile directory"))
}
