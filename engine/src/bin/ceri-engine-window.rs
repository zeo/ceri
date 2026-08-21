#[cfg(feature = "native-http")]
use ceri_engine::NativeHttpProvider;
use ceri_engine::{APP_ORIGIN, Browser, ResourceBundle, WindowConfig, run_window};
use std::env;
#[cfg(feature = "native-http")]
use std::rc::Rc;

fn main() -> Result<(), String> {
    let native_base = parse_native_base(env::args().skip(1))?;
    let config = WindowConfig {
        title: "Ceri custom engine".to_string(),
        width: 800,
        height: 520,
        start_url: format!("{APP_ORIGIN}/"),
        ..WindowConfig::default()
    };

    if let Some(base) = native_base {
        #[cfg(feature = "native-http")]
        {
            return run_window(config, move || {
                let provider =
                    NativeHttpProvider::new(base.clone()).map_err(|error| error.to_string())?;
                Ok(Browser::with_resource_provider(
                    ResourceBundle::default(),
                    Rc::new(provider),
                ))
            });
        }
        #[cfg(not(feature = "native-http"))]
        {
            let _ = base;
            return Err(
                "--native-base requires the native-http feature: cargo run --features \"windowed native-http\""
                    .to_string(),
            );
        }
    }

    run_window(config, || {
        let mut resources = ResourceBundle::from_directory(env!("CARGO_MANIFEST_DIR"))
            .map_err(|error| format!("could not load packaged engine resources: {error}"))?;
        resources
            .insert(
                "/index.html",
                "text/html; charset=utf-8",
                include_bytes!("../../demo.html").to_vec(),
            )
            .map_err(|error| error.to_string())?;
        Ok(Browser::new(resources))
    })
}

fn parse_native_base<I>(arguments: I) -> Result<Option<String>, String>
where
    I: IntoIterator<Item = String>,
{
    let mut arguments = arguments.into_iter();
    let mut native_base = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--native-base" => {
                if native_base.is_some() {
                    return Err("--native-base may only be provided once".to_string());
                }
                native_base = Some(
                    arguments
                        .next()
                        .ok_or_else(|| "--native-base requires an HTTPS URL".to_string())?,
                );
            }
            "--help" | "-h" => {
                println!(
                    "usage: ceri-engine-window [--native-base HTTPS_URL]\n\nWithout --native-base, the packaged engine demo is shown. With it, the engine worker loads the app origin through the optional native HTTPS provider."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(native_base)
}

#[cfg(test)]
mod tests {
    use super::parse_native_base;

    #[test]
    fn parses_optional_native_base() {
        assert_eq!(
            parse_native_base(vec![
                "--native-base".to_string(),
                "https://example.test".to_string()
            ])
            .expect("native base should parse")
            .as_deref(),
            Some("https://example.test")
        );
    }

    #[test]
    fn rejects_unknown_window_arguments() {
        assert!(parse_native_base(vec!["--unknown".to_string()]).is_err());
    }
}
