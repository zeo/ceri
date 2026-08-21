use std::{
    env,
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

const APP_NAME: &str = "ceri";
#[cfg(all(feature = "cef-engine", target_os = "macos"))]
const HELPER_NAME: &str = "ceri_helper";

#[cfg(feature = "cef-engine")]
const HELP: &str = "Usage: ceri_bundle [--release] [--output PATH]\n\
\n\
Build Ceri and assemble its bundled CEF runtime.\n\
\n\
Options:\n\
  -o, --output PATH  Bundle destination, defaults to ./dist\n\
      --release      Build the release profile\n\
  -h, --help         Show this help\n";

#[cfg(not(feature = "cef-engine"))]
const HELP: &str = "Usage: ceri_bundle [--release] [--output PATH]\n\
\n\
Build Ceri on its own engine and assemble the app bundle.\n\
\n\
Options:\n\
  -o, --output PATH  Bundle destination, defaults to ./dist\n\
      --release      Build the release profile\n\
  -h, --help         Show this help\n";

#[derive(Debug, Eq, PartialEq)]
struct Options {
    output: PathBuf,
    release: bool,
}

#[derive(Debug)]
enum Action {
    Bundle(Options),
    Help,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ceri_bundle: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let Action::Bundle(options) = parse_args(env::args_os().skip(1))? else {
        print!("{HELP}");
        return Ok(());
    };

    let output = prepare_output(&options.output)?;
    let executable = platform_bundle(&output, options.release)?;
    copy_app_assets(&output)?;
    println!("{}", executable.display());
    Ok(())
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> io::Result<Action> {
    let mut output = None;
    let mut release = false;
    let mut args = args.into_iter();

    while let Some(argument) = args.next() {
        match argument.as_os_str() {
            value if value == OsStr::new("-h") || value == OsStr::new("--help") => {
                return Ok(Action::Help);
            }
            value if value == OsStr::new("--release") => release = true,
            value if value == OsStr::new("-o") || value == OsStr::new("--output") => {
                if output.is_some() {
                    return Err(invalid_input("--output may only be passed once"));
                }
                output = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| invalid_input("--output requires a path"))?,
                ));
            }
            _ => {
                return Err(invalid_input(format!(
                    "unknown argument {}",
                    argument.to_string_lossy()
                )));
            }
        }
    }

    Ok(Action::Bundle(Options {
        output: output.unwrap_or_else(|| PathBuf::from("dist")),
        release,
    }))
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn prepare_output(requested: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(requested)?;
    let output = requested.canonicalize()?;
    if output.parent().is_none() {
        return Err(invalid_input(
            "the bundle destination cannot be a filesystem root",
        ));
    }

    let working_directory = env::current_dir()?.canonicalize()?;
    if output == working_directory {
        return Err(invalid_input(
            "the bundle destination cannot be the project directory",
        ));
    }

    if std::fs::read_dir(&output)?.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the bundle destination must be empty",
        ));
    }

    #[cfg(target_os = "macos")]
    if output.join(format!("{APP_NAME}.app")).exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the destination already contains a Ceri app bundle",
        ));
    }

    Ok(output)
}

fn copy_app_assets(output: &Path) -> io::Result<()> {
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for required in ["web/index.html", "sdk/index.js", "sdk/index.d.ts"] {
        if !project.join(required).is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("required app asset is missing: {required}"),
            ));
        }
    }

    let assets = output.join("assets");
    std::fs::create_dir(&assets)?;
    for directory in ["web", "sdk"] {
        copy_tree(&project.join(directory), &assets.join(directory))?;
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let destination = destination.join(entry.file_name());
        if file_type.is_symlink() {
            return Err(invalid_input(format!(
                "app assets cannot contain symbolic links: {}",
                entry.path().display()
            )));
        }
        if file_type.is_dir() {
            copy_tree(&entry.path(), &destination)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}

#[cfg(all(feature = "cef-engine", target_os = "windows"))]
fn platform_bundle(output: &Path, release: bool) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(cef::build_util::win::build_bundle(
        output, APP_NAME, release,
    )?)
}

#[cfg(all(feature = "cef-engine", target_os = "linux"))]
fn platform_bundle(output: &Path, release: bool) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(cef::build_util::linux::build_bundle(
        output, APP_NAME, release,
    )?)
}

#[cfg(all(feature = "cef-engine", target_os = "macos"))]
fn platform_bundle(output: &Path, release: bool) -> Result<PathBuf, Box<dyn std::error::Error>> {
    use cef::build_util::{mac, metadata};

    let metadata = metadata::get_cargo_metadata()?;
    let bundle_metadata = metadata.parse_bundle_metadata(APP_NAME)?;
    if bundle_metadata.helper_name != HELPER_NAME {
        return Err(invalid_input(format!(
            "package metadata must name {HELPER_NAME} as the CEF helper"
        ))
        .into());
    }

    cargo_build(APP_NAME, release, &[])?;
    cargo_build(HELPER_NAME, release, &[])?;

    let profile = if release { "release" } else { "debug" };
    let target = metadata.target_directory().join(profile);
    let version = env!("CARGO_PKG_VERSION").parse()?;
    let info = mac::BundleInfo::new(APP_NAME, "dev.ceri.runtime", "Ceri", "English", version);

    Ok(mac::bundle(
        output,
        &target,
        APP_NAME,
        HELPER_NAME,
        bundle_metadata.resources_path,
        info,
    )?)
}

fn cargo_build(binary: &str, release: bool, features: &[&str]) -> io::Result<()> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut command = Command::new(cargo);
    command.args(["build", "--bin", binary]);
    if !features.is_empty() {
        command.arg("--no-default-features");
        command.args(["--features", &features.join(" ")]);
    }
    if release {
        command.arg("--release");
    }

    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "cargo could not build the {binary} binary"
        )))
    }
}

#[cfg(all(
    feature = "cef-engine",
    not(any(target_os = "windows", target_os = "linux", target_os = "macos"))
))]
fn platform_bundle(_output: &Path, _release: bool) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "CEF desktop bundles support Windows, Linux, and macOS",
    )
    .into())
}

// the custom engine links no external runtime, so the bundle is the executable plus its assets
#[cfg(not(feature = "cef-engine"))]
fn platform_bundle(output: &Path, release: bool) -> Result<PathBuf, Box<dyn std::error::Error>> {
    cargo_build(APP_NAME, release, &["custom-engine"])?;
    let profile = if release { "release" } else { "debug" };
    let built = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(profile)
        .join(executable_name(APP_NAME));
    if !built.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("cargo did not produce {}", built.display()),
        )
        .into());
    }
    let executable = output.join(executable_name(APP_NAME));
    std::fs::copy(&built, &executable)?;
    Ok(executable)
}

#[cfg(not(feature = "cef-engine"))]
fn executable_name(name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_debug_dist_bundle() {
        let Action::Bundle(options) = parse_args([]).unwrap() else {
            panic!("expected bundle options");
        };

        assert_eq!(
            options,
            Options {
                output: PathBuf::from("dist"),
                release: false,
            }
        );
    }

    #[test]
    fn accepts_release_and_output() {
        let Action::Bundle(options) = parse_args([
            OsString::from("--release"),
            OsString::from("--output"),
            OsString::from("artifacts/ceri"),
        ])
        .unwrap() else {
            panic!("expected bundle options");
        };

        assert_eq!(
            options,
            Options {
                output: PathBuf::from("artifacts/ceri"),
                release: true,
            }
        );
    }

    #[test]
    fn rejects_unknown_arguments() {
        let error = parse_args([OsString::from("--format")]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn refuses_to_replace_a_nonempty_bundle() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            env::temp_dir().join(format!("ceri-bundle-test-{}-{nonce}", std::process::id(),));
        std::fs::create_dir_all(&directory).unwrap();
        let marker = directory.join("keep.txt");
        std::fs::write(&marker, b"keep").unwrap();

        let error = prepare_output(&directory).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&marker).unwrap(), b"keep");

        std::fs::remove_file(marker).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
