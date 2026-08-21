use crate::assets::{AssetOrigin, AssetRoute, RequestTarget};
use crate::fonts::SystemFonts;
use crate::launch::{self, ASSETS_SWITCH, ENTRY_SWITCH};
use ceri_engine::{
    AppUrl, Browser, EngineHost, FontSet, HostRuntimeInfo, NavigationError, Resource,
    ResourceBundle, ResourceProvider, Size, WindowConfig, mime_type_for, run_window,
};
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const SCREENSHOT_SWITCH: &str = "ceri-screenshot";

pub fn run() -> Result<(), String> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let launch = launch::resolve(
        launch::switch_value(arguments.clone(), ASSETS_SWITCH).map(PathBuf::from),
        launch::switch_value(arguments.clone(), ENTRY_SWITCH),
    )?;
    let storage = launch::profile_path().map(|profile| profile.join("storage"))?;
    let assets = launch.assets.clone();
    let config = WindowConfig {
        title: "Ceri".to_string(),
        start_url: launch.initial_url.clone(),
        ..WindowConfig::default()
    };
    if let Some(path) = launch::switch_value(arguments, SCREENSHOT_SWITCH) {
        return capture(&config, PathBuf::from(path), assets, storage);
    }
    run_window(config, move || build_browser(assets, storage))
}

// renders one frame without opening a window, for smoke checks and release shots
fn capture(
    config: &WindowConfig,
    output: PathBuf,
    assets: Arc<AssetOrigin>,
    storage: PathBuf,
) -> Result<(), String> {
    let mut host = EngineHost::new(
        build_browser(assets, storage)?,
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

fn build_browser(assets: Arc<AssetOrigin>, storage: PathBuf) -> Result<Browser, String> {
    fs::create_dir_all(&storage)
        .map_err(|error| format!("could not create {}: {error}", storage.display()))?;
    let browser = Browser::with_resource_provider(
        ResourceBundle::default(),
        Rc::new(PackagedAssets { assets }),
    )
    .with_storage_directory(&storage)
    .map_err(|error| {
        format!(
            "could not open the app storage bucket {}: {error}",
            storage.display()
        )
    })?
    .with_host_runtime_info(runtime_info())
    .with_fonts(Rc::new(system_font_set()));
    Ok(browser)
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

// serves the packaged app through the same origin policy the CEF scheme handler uses
struct PackagedAssets {
    assets: Arc<AssetOrigin>,
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
}

fn is_document_path(path: &str) -> bool {
    let last = path.rsplit('/').next().unwrap_or_default();
    !last.contains('.') || last.ends_with(".html") || last.ends_with(".htm")
}

#[cfg(test)]
mod tests {
    use super::{PackagedAssets, is_document_path};
    use crate::assets::AssetOrigin;
    use ceri_engine::{AppUrl, ResourceProvider};
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
}
