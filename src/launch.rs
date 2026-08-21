use crate::assets::{APP_ORIGIN, AssetOrigin, AssetRoute, RequestTarget};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const ENTRY_SWITCH: &str = "ceri-entry";
pub const ASSETS_SWITCH: &str = "ceri-assets";

#[derive(Clone)]
pub struct LaunchConfig {
    pub assets: Arc<AssetOrigin>,
    pub initial_url: String,
}

pub fn resolve(asset_root: Option<PathBuf>, entry: Option<String>) -> Result<LaunchConfig, String> {
    let asset_root = asset_root.unwrap_or_else(default_asset_root);
    let assets = AssetOrigin::new(&asset_root).map_err(|error| {
        format!(
            "asset root {} is unavailable: {error}",
            asset_root.display()
        )
    })?;

    let entry = entry.unwrap_or_else(|| detect_entry(assets.root()).to_string());
    let entry = normalized_entry(&entry)?;
    let initial_url = format!("{APP_ORIGIN}{entry}");
    match assets
        .route(&initial_url, RequestTarget::TopLevelNavigation)
        .map_err(|error| format!("could not inspect app entry: {error}"))?
    {
        AssetRoute::File(_) | AssetRoute::AppShell(_) => {}
        AssetRoute::NotFound => {
            return Err(format!(
                "app entry {entry} does not exist under {}",
                assets.root().display()
            ));
        }
        AssetRoute::Rejected(reason) => {
            return Err(format!("app entry {entry} is unsafe: {reason:?}"));
        }
    }

    Ok(LaunchConfig {
        assets: Arc::new(assets),
        initial_url,
    })
}

pub fn switch_value<I>(args: I, name: &str) -> Option<String>
where
    I: IntoIterator<Item = OsString>,
{
    let exact = format!("--{name}");
    let prefix = format!("{exact}=");
    let args: Vec<String> = args
        .into_iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    for (index, argument) in args.iter().enumerate() {
        if let Some(value) = argument.strip_prefix(&prefix)
            && !value.is_empty()
        {
            return Some(value.to_string());
        }
        if argument == &exact {
            let value = args.get(index + 1)?;
            if !value.starts_with("--") && !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

pub fn normalized_entry(entry: &str) -> Result<String, String> {
    if entry.contains("://") || entry.contains(['?', '#', '\0']) {
        return Err("app entry must be an absolute origin path".to_string());
    }
    Ok(if entry.starts_with('/') {
        entry.to_string()
    } else {
        format!("/{entry}")
    })
}

pub fn detect_entry(asset_root: &Path) -> &'static str {
    if asset_root.join("index.html").is_file() {
        "/index.html"
    } else {
        "/web/index.html"
    }
}

pub fn default_asset_root() -> PathBuf {
    let beside_executable = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("assets")));
    if let Some(path) = beside_executable.filter(|path| path.is_dir()) {
        return path;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn profile_path() -> Result<PathBuf, String> {
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
        .ok_or_else(|| "could not resolve a per-user profile directory".to_string())
}

#[cfg(test)]
mod tests {
    use super::{normalized_entry, switch_value};
    use std::ffi::OsString;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn reads_switch_values_in_both_forms() {
        assert_eq!(
            switch_value(args(&["--ceri-assets=D:/app/web"]), "ceri-assets").as_deref(),
            Some("D:/app/web")
        );
        assert_eq!(
            switch_value(args(&["--ceri-assets", "D:/app/web"]), "ceri-assets").as_deref(),
            Some("D:/app/web")
        );
        assert_eq!(
            switch_value(
                args(&["--ceri-assets", "--ceri-entry=/x.html"]),
                "ceri-assets"
            ),
            None
        );
        assert_eq!(switch_value(args(&["--other"]), "ceri-assets"), None);
    }

    #[test]
    fn rejects_entries_that_are_not_origin_paths() {
        assert_eq!(normalized_entry("app.html").as_deref(), Ok("/app.html"));
        assert_eq!(normalized_entry("/app.html").as_deref(), Ok("/app.html"));
        assert!(normalized_entry("https://example.test/app.html").is_err());
        assert!(normalized_entry("/app.html?query=1").is_err());
    }
}
