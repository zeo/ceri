#[cfg(target_os = "linux")]
mod deb;
#[cfg(target_os = "windows")]
mod setup;

use serde::Deserialize;
use std::{
    collections::BTreeMap,
    env,
    error::Error,
    ffi::{OsStr, OsString},
    io,
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode},
};

const HELP: &str = "Usage: ceri_bundle [--debug] [--project PATH] [--output PATH]\n\
\n\
Build a ceri app and package it: an app folder, and beside it a per-user setup\n\
executable on Windows or a Debian package on Linux. The project folder holds the\n\
app's Cargo.toml and its ceri.json.\n\
\n\
Options:\n\
  -p, --project PATH  App project, defaults to the current directory\n\
  -o, --output PATH   Bundle destination, defaults to ./dist\n\
      --debug         Build the debug profile instead of release\n\
  -h, --help          Show this help\n";

#[derive(Debug, Eq, PartialEq)]
struct Options {
    output: PathBuf,
    project: PathBuf,
    release: bool,
}

#[derive(Debug)]
enum Action {
    Bundle(Options),
    Help,
}

// what an app's ceri.json says about it; the rest comes from its Cargo.toml
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    identifier: String,
    name: Option<String>,
    binary: Option<String>,
    publisher: Option<String>,
    #[serde(default)]
    icons: Vec<PathBuf>,
    #[serde(default)]
    resources: BTreeMap<PathBuf, PathBuf>,
    #[serde(default)]
    sign_command: Vec<String>,
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    target_directory: PathBuf,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
    description: Option<String>,
    #[serde(default)]
    authors: Vec<String>,
    manifest_path: PathBuf,
    targets: Vec<Target>,
}

#[derive(Deserialize)]
struct Target {
    name: String,
    kind: Vec<String>,
}

struct Project {
    manifest: PathBuf,
    target_directory: PathBuf,
    identifier: String,
    name: String,
    version: String,
    description: Option<String>,
    // the .deb names its maintainer from these
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    authors: Vec<String>,
    publisher: Option<String>,
    binary: String,
    icons: Vec<PathBuf>,
    resources: Vec<(PathBuf, PathBuf)>,
    sign_command: Vec<String>,
}

fn main() -> ExitCode {
    #[cfg(target_os = "windows")]
    if let Some(code) = setup::run_packed() {
        return code;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ceri_bundle: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let Action::Bundle(options) = parse_args(env::args_os().skip(1))? else {
        print!("{HELP}");
        return Ok(());
    };

    let project = Project::load(&options.project)?;
    let output = prepare_output(&options.output)?;
    let built = build(&project, options.release)?;
    let app = output.join("app");
    std::fs::create_dir(&app)?;
    let executable = app.join(executable_name(&project.binary));
    std::fs::copy(&built, &executable)?;
    for (source, destination) in &project.resources {
        let destination = app.join(destination);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if source.is_dir() {
            copy_tree(source, &destination)?;
        } else {
            std::fs::copy(source, &destination)?;
        }
    }
    project.sign(&executable)?;
    println!("{}", executable.display());
    #[cfg(target_os = "windows")]
    println!("{}", setup::write(&project, &app, &output)?.display());
    #[cfg(target_os = "linux")]
    println!("{}", deb::write(&project, &app, &output)?.display());
    Ok(())
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> io::Result<Action> {
    let mut output = None;
    let mut project = None;
    let mut release = true;
    let mut args = args.into_iter();

    while let Some(argument) = args.next() {
        let slot = match argument.to_str() {
            Some("-h" | "--help") => return Ok(Action::Help),
            Some("--debug") => {
                release = false;
                continue;
            }
            Some("-o" | "--output") => &mut output,
            Some("-p" | "--project") => &mut project,
            _ => {
                return Err(invalid_input(format!(
                    "unknown argument {}",
                    argument.to_string_lossy()
                )));
            }
        };
        let flag = argument.to_string_lossy();
        if slot.is_some() {
            return Err(invalid_input(format!("{flag} may only be passed once")));
        }
        let value = args
            .next()
            .ok_or_else(|| invalid_input(format!("{flag} requires a path")))?;
        *slot = Some(PathBuf::from(value));
    }

    Ok(Action::Bundle(Options {
        output: output.unwrap_or_else(|| PathBuf::from("dist")),
        project: project.unwrap_or_else(|| PathBuf::from(".")),
        release,
    }))
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

impl Project {
    fn load(root: &Path) -> Result<Self, Box<dyn Error>> {
        // absolute rather than canonical paths, since tools like signtool may not take the
        // verbatim form canonicalize gives on windows
        let root = std::path::absolute(root)?;
        let manifest = root.join("Cargo.toml");
        let canonical = manifest.canonicalize()?;
        let config_path = root.join("ceri.json");
        let config = std::fs::read(&config_path)
            .map_err(|error| format!("cannot read {}: {error}", config_path.display()))?;
        let config: Config = serde_json::from_slice(&config)
            .map_err(|error| format!("{}: {error}", config_path.display()))?;

        let metadata = Command::new(cargo())
            .args([
                "metadata",
                "--no-deps",
                "--format-version",
                "1",
                "--manifest-path",
            ])
            .arg(&manifest)
            .output()?;
        if !metadata.status.success() {
            return Err(format!(
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&metadata.stderr).trim()
            )
            .into());
        }
        let metadata: Metadata = serde_json::from_slice(&metadata.stdout)?;
        let package = metadata
            .packages
            .into_iter()
            .find(|package| package.manifest_path.canonicalize().ok().as_ref() == Some(&canonical))
            .ok_or("the project's Cargo.toml has no [package]")?;

        let binaries: Vec<_> = package
            .targets
            .iter()
            .filter(|target| target.kind.iter().any(|kind| kind == "bin"))
            .map(|target| target.name.as_str())
            .collect();
        let binary = match (config.binary, binaries.as_slice()) {
            (Some(binary), _) if binaries.contains(&binary.as_str()) => binary,
            (Some(binary), _) => return Err(format!("the package has no binary {binary}").into()),
            (None, [binary]) => binary.to_string(),
            (None, _) => {
                return Err(
                    "the package has more than one binary, so ceri.json names one as \"binary\""
                        .into(),
                );
            }
        };

        check_identifier(&config.identifier)?;
        let name = config.name.unwrap_or_else(|| package.name.clone());
        check_name(&name)?;
        let publisher = config.publisher.or_else(|| {
            let author = package.authors.first()?;
            Some(
                author
                    .split('<')
                    .next()
                    .unwrap_or(author)
                    .trim()
                    .to_string(),
            )
        });
        let resources = config
            .resources
            .into_iter()
            .map(|(source, destination)| {
                let inside = destination
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)));
                match inside && destination.components().next().is_some() {
                    true => Ok((root.join(source), destination)),
                    false => Err(format!(
                        "the resource destination {} must be a relative path inside the app",
                        destination.display()
                    )),
                }
            })
            .collect::<Result<_, _>>()?;

        Ok(Self {
            manifest,
            target_directory: metadata.target_directory,
            identifier: config.identifier,
            name,
            version: package.version,
            description: package.description,
            authors: package.authors,
            publisher,
            binary,
            icons: config.icons.iter().map(|icon| root.join(icon)).collect(),
            resources,
            sign_command: config.sign_command,
        })
    }

    #[cfg(target_os = "windows")]
    fn icon(&self, extension: &str) -> Option<&Path> {
        self.icons
            .iter()
            .find(|icon| {
                icon.extension()
                    .is_some_and(|found| found.eq_ignore_ascii_case(extension))
            })
            .map(PathBuf::as_path)
    }

    // the sign command runs on each executable the bundle ships, with {file} standing for it
    fn sign(&self, file: &Path) -> Result<(), Box<dyn Error>> {
        let Some((program, arguments)) = self.sign_command.split_first() else {
            return Ok(());
        };
        let status = Command::new(program)
            .args(arguments.iter().map(|argument| match argument.as_str() {
                "{file}" => file.as_os_str(),
                other => OsStr::new(other),
            }))
            .status()?;
        match status.success() {
            true => Ok(()),
            false => Err(format!("the sign command failed on {}", file.display()).into()),
        }
    }
}

// the same rule the runtime holds the identifier to, since it names folders and registry keys
fn check_identifier(identifier: &str) -> Result<(), String> {
    let usable = !identifier.is_empty()
        && identifier.len() <= 128
        && !identifier.starts_with('.')
        && !identifier.ends_with('.')
        && identifier
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character));
    match usable {
        true => Ok(()),
        false => Err(format!(
            "the identifier {identifier:?} must be letters, digits, dots, hyphens and underscores"
        )),
    }
}

// the name becomes the install folder and the start menu entry
fn check_name(name: &str) -> Result<(), String> {
    let usable = !name.trim().is_empty()
        && !name.ends_with(['.', ' '])
        && !name
            .chars()
            .any(|character| character.is_control() || "<>:\"/\\|?*".contains(character));
    match usable {
        true => Ok(()),
        false => Err(format!(
            "the name {name:?} cannot hold control characters, <>:\"/\\|?* or end in a dot or space"
        )),
    }
}

fn cargo() -> OsString {
    env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

fn prepare_output(requested: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(requested)?;
    let output = std::path::absolute(requested)?;
    if output.canonicalize()?.parent().is_none() {
        return Err(invalid_input(
            "the bundle destination cannot be a filesystem root",
        ));
    }

    let working_directory = env::current_dir()?.canonicalize()?;
    if output.canonicalize()? == working_directory {
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

    Ok(output)
}

fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let destination = destination.join(entry.file_name());
        if file_type.is_symlink() {
            return Err(invalid_input(format!(
                "app resources cannot contain symbolic links: {}",
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

// every file under the app folder as its path there, with forward slashes, in a stable order
fn app_files(app: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let mut files = Vec::new();
    let mut pending = vec![app.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(app)
                .expect("the walk stays under the app folder")
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            files.push((relative, path));
        }
    }
    files.sort();
    Ok(files)
}

fn build(project: &Project, release: bool) -> Result<PathBuf, Box<dyn Error>> {
    let mut command = Command::new(cargo());
    command
        .args(["build", "--bin", &project.binary, "--manifest-path"])
        .arg(&project.manifest);
    if release {
        command.arg("--release");
    }
    if !command.status()?.success() {
        return Err(format!("cargo could not build the {} binary", project.binary).into());
    }

    let built = project
        .target_directory
        .join(if release { "release" } else { "debug" })
        .join(executable_name(&project.binary));
    if !built.is_file() {
        return Err(format!("cargo did not produce {}", built.display()).into());
    }
    Ok(built)
}

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
    fn defaults_to_a_release_bundle_of_the_current_project() {
        let Action::Bundle(options) = parse_args([]).unwrap() else {
            panic!("expected bundle options");
        };

        assert_eq!(
            options,
            Options {
                output: PathBuf::from("dist"),
                project: PathBuf::from("."),
                release: true,
            }
        );
    }

    #[test]
    fn accepts_debug_project_and_output() {
        let Action::Bundle(options) = parse_args([
            OsString::from("--debug"),
            OsString::from("-p"),
            OsString::from("../clip"),
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
                project: PathBuf::from("../clip"),
                release: false,
            }
        );
    }

    #[test]
    fn rejects_unknown_and_repeated_arguments() {
        let error = parse_args([OsString::from("--format")]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let error = parse_args(["-o", "a", "-o", "b"].map(OsString::from)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn names_and_identifiers_stay_usable_as_paths() {
        assert!(check_identifier("lt.rot.clip").is_ok());
        assert!(check_identifier("lt/rot").is_err());
        assert!(check_name("Clip Recorder").is_ok());
        assert!(check_name("clip: beta").is_err());
        assert!(check_name("clip.").is_err());
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
