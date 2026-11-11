use crate::assets::{APP_ORIGIN, AssetOrigin, AssetRoute, EmbeddedAssets, RequestTarget};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

pub const ENTRY_SWITCH: &str = "ceri-entry";
pub const ASSETS_SWITCH: &str = "ceri-assets";

#[derive(Clone)]
pub struct LaunchConfig {
    pub assets: Arc<AssetOrigin>,
    pub initial_url: String,
}

// a folder named on the command line wins over the app's compiled-in files
pub fn resolve(
    asset_root: Option<PathBuf>,
    embedded: Option<EmbeddedAssets>,
    entry: Option<String>,
) -> Result<LaunchConfig, String> {
    let assets = match (asset_root, embedded) {
        (None, Some(files)) => AssetOrigin::embedded(files),
        (asset_root, _) => {
            let asset_root = asset_root.unwrap_or_else(default_asset_root);
            AssetOrigin::new(&asset_root).map_err(|error| {
                format!(
                    "asset root {} is unavailable: {error}",
                    asset_root.display()
                )
            })?
        }
    };

    let entry = entry.unwrap_or_else(|| detect_entry(&assets).to_string());
    let entry = normalized_entry(&entry)?;
    let initial_url = format!("{APP_ORIGIN}{entry}");
    match assets
        .route(&initial_url, RequestTarget::TopLevelNavigation)
        .map_err(|error| format!("could not inspect app entry: {error}"))?
    {
        AssetRoute::File(_) | AssetRoute::AppShell(_) => {}
        AssetRoute::NotFound => {
            return Err(match assets.root() {
                Some(root) => format!("app entry {entry} does not exist under {}", root.display()),
                None => format!("app entry {entry} is not among the embedded assets"),
            });
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

pub fn detect_entry(assets: &AssetOrigin) -> &'static str {
    match assets.route(
        &format!("{APP_ORIGIN}/index.html"),
        RequestTarget::Subresource,
    ) {
        Ok(AssetRoute::File(_)) => "/index.html",
        _ => "/web/index.html",
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

// each app keeps its pages' storage and window state under its own identifier
pub fn profile_path(identifier: &str) -> Result<PathBuf, String> {
    crate::dirs::local_data()
        .map(|path| path.join(identifier).join("profile"))
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
