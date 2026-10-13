use crate::assets::{AssetOrigin, AssetRoute, RequestTarget};
use crate::desktop::{DesktopClipboard, DesktopFilePicker};
use crate::fonts::SystemFonts;
use crate::launch::{self, ASSETS_SWITCH, ENTRY_SWITCH};
use ceri_engine::{
    AppUrl, Browser, EngineHost, ExternalRequest, FontSet, HostRuntimeInfo, NavigationError,
    Resource, ResourceBundle, ResourceProvider, ResourceResponse, Size, WindowConfig,
    mime_type_for, run_window,
};
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const SCREENSHOT_SWITCH: &str = "ceri-screenshot";
const ALLOW_ORIGIN_SWITCH: &str = "ceri-allow-origin";

pub fn run() -> Result<(), String> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let launch = launch::resolve(
        launch::switch_value(arguments.clone(), ASSETS_SWITCH).map(PathBuf::from),
        launch::switch_value(arguments.clone(), ENTRY_SWITCH),
    )?;
    let storage = launch::profile_path().map(|profile| profile.join("storage"))?;
    let allowed = allowed_origins(&arguments)?;
    let assets = launch.assets.clone();
    let config = WindowConfig {
        title: "Ceri".to_string(),
        start_url: launch.initial_url.clone(),
        icon: app_icon(launch.assets.as_ref()),
        geometry_path: launch::profile_path()
            .ok()
            .map(|profile| profile.join("window")),
        ..WindowConfig::default()
    };
    if let Some(path) = launch::switch_value(arguments, SCREENSHOT_SWITCH) {
        return capture(&config, PathBuf::from(path), assets, storage, allowed);
    }
    run_window(config, move || {
        build_browser(assets.clone(), storage.clone(), allowed.clone())
    })
}

// renders one frame without opening a window, for smoke checks and release shots
fn capture(
    config: &WindowConfig,
    output: PathBuf,
    assets: Arc<AssetOrigin>,
    storage: PathBuf,
    allowed: Vec<String>,
) -> Result<(), String> {
    let mut host = EngineHost::new(
        build_browser(assets, storage, allowed)?,
        Size {
            width: config.width as f32,
            height: config.height as f32,
        },
    );
    let status = host
        .navigate(&config.start_url)
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
    for log in host.browser().console_logs() {
        println!("console: {log}");
    }
    Ok(())
}

fn build_browser(
    assets: Arc<AssetOrigin>,
    storage: PathBuf,
    allowed_origins: Vec<String>,
) -> Result<Browser, String> {
    fs::create_dir_all(&storage)
        .map_err(|error| format!("could not create {}: {error}", storage.display()))?;
    let browser = Browser::with_resource_provider(
        ResourceBundle::default(),
        Rc::new(PackagedAssets {
            assets,
            allowed_origins,
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
    .with_fonts(Rc::new(system_font_set()))
    .with_clipboard_provider(Rc::new(DesktopClipboard))
    .with_file_picker(Rc::new(DesktopFilePicker));
    Ok(browser)
}

// the packaged favicon becomes the window and taskbar icon
fn app_icon(assets: &AssetOrigin) -> Option<(Vec<u8>, u32)> {
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
        let source = fs::read(&asset.path).ok()?;
        if let Some(icon) = ceri_engine::rasterize_icon(&source, 64) {
            return Some(icon);
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
        "none",
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

// serves the packaged app through the same origin policy the CEF scheme handler uses
struct PackagedAssets {
    assets: Arc<AssetOrigin>,
    allowed_origins: Vec<String>,
}

impl ResourceProvider for PackagedAssets {
    fn provide(&self, url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
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
        let body = fs::read(&asset.path).map_err(|error| {
            NavigationError::ResourceProvider(format!(
                "could not read {}: {error}",
                asset.path.display()
            ))
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
}

fn is_document_path(path: &str) -> bool {
    let last = path.rsplit('/').next().unwrap_or_default();
    !last.contains('.') || last.ends_with(".html") || last.ends_with(".htm")
}

#[cfg(test)]
mod tests {
    use super::{PackagedAssets, allowed_origins, is_document_path};
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

    #[test]
    fn serves_packaged_files_and_reports_missing_ones() {
        let root = std::env::current_dir()
            .expect("working directory should resolve")
            .join("web");
        let provider = PackagedAssets {
            assets: Arc::new(AssetOrigin::new(&root).expect("web root should exist")),
            allowed_origins: Vec::new(),
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
    }

    #[test]
    fn external_fetches_need_an_allowed_origin() {
        let root = std::env::current_dir()
            .expect("working directory should resolve")
            .join("web");
        let provider = PackagedAssets {
            assets: Arc::new(AssetOrigin::new(&root).expect("web root should exist")),
            allowed_origins: vec!["https://api.example.test".to_string()],
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
}
