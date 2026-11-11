use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

// the folders whose files pages may load through files URLs, which carry the file's absolute
// path: /D/Videos/a.mp4 for D:\Videos\a.mp4, /UNC/server/share/a.mp4 for a share, and the path
// itself elsewhere
#[derive(Clone, Default)]
pub struct FileScope {
    folders: Arc<RwLock<Vec<PathBuf>>>,
}

impl FileScope {
    // the folder and everything under it, whether or not it exists yet
    pub fn allow(&self, folder: impl Into<PathBuf>) -> Result<(), String> {
        let folder = folder.into();
        if !folder.is_absolute() {
            return Err(format!(
                "a file scope needs an absolute folder, not {}",
                folder.display()
            ));
        }
        let mut folders = self.folders.write().unwrap_or_else(PoisonError::into_inner);
        if !folders.contains(&folder) {
            folders.push(folder);
        }
        Ok(())
    }

    pub fn revoke(&self, folder: &Path) {
        self.folders
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|allowed| allowed != folder);
    }

    // the file a files URL path names, when it is a file inside an allowed folder
    pub fn resolve(&self, url_path: &str) -> Option<PathBuf> {
        let real = file_path(url_path)?.canonicalize().ok()?;
        let folders = self.folders.read().unwrap_or_else(PoisonError::into_inner);
        // comparing resolved paths keeps links inside a folder from leading out of it
        let inside = folders.iter().any(|folder| {
            folder
                .canonicalize()
                .is_ok_and(|folder| real.starts_with(folder))
        });
        (inside && real.is_file()).then_some(real)
    }
}

fn file_path(url_path: &str) -> Option<PathBuf> {
    let mut segments = url_path.strip_prefix('/')?.split('/');
    let mut path = root(&mut segments)?;
    for segment in segments {
        // a colon would name an alternate stream on windows
        let unsafe_segment = segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.contains('\\')
            || (cfg!(windows) && segment.contains(':'));
        if unsafe_segment {
            return None;
        }
        path.push(segment);
    }
    Some(path)
}

#[cfg(windows)]
fn root<'a>(segments: &mut impl Iterator<Item = &'a str>) -> Option<PathBuf> {
    match segments.next()? {
        "UNC" => {
            let (server, share) = (segments.next()?, segments.next()?);
            let usable =
                |part: &str| !matches!(part, "" | "." | "..") && !part.contains(['\\', ':']);
            (usable(server) && usable(share))
                .then(|| PathBuf::from(format!(r"\\{server}\{share}\")))
        }
        drive if drive.len() == 1 && drive.as_bytes()[0].is_ascii_alphabetic() => {
            Some(PathBuf::from(format!(r"{drive}:\")))
        }
        _ => None,
    }
}

#[cfg(not(windows))]
fn root<'a>(_segments: &mut impl Iterator<Item = &'a str>) -> Option<PathBuf> {
    Some(PathBuf::from("/"))
}

#[cfg(test)]
mod tests {
    use super::FileScope;
    use ceri_engine::AppUrl;
    use std::fs;
    use std::path::Path;

    // what the sdk's fileSrc builds from a path
    fn file_src(path: &Path) -> String {
        let text = path.to_str().expect("a unicode path");
        let (prefix, rest) = match text.as_bytes() {
            [drive, b':', b'\\', ..] => (vec![(*drive as char).to_string()], &text[3..]),
            _ => (Vec::new(), text.trim_start_matches('/')),
        };
        let parts = prefix
            .into_iter()
            .chain(rest.split(['\\', '/']).map(|part| {
                part.bytes()
                    .map(|byte| match byte {
                        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                            (byte as char).to_string()
                        }
                        _ => format!("%{byte:02X}"),
                    })
                    .collect::<String>()
            }))
            .collect::<Vec<_>>();
        format!("https://files.ceri.invalid/{}", parts.join("/"))
    }

    #[test]
    fn serves_files_inside_allowed_folders_and_nothing_outside() {
        let root = std::env::temp_dir().join(format!("ceri-scope-{}", std::process::id()));
        let shots = root.join("shots");
        fs::create_dir_all(shots.join("2026 11")).expect("allowed folder");
        let file = shots.join("2026 11").join("a#1 é.png");
        fs::write(&file, b"png").expect("allowed file");
        fs::write(root.join("secret.txt"), b"secret").expect("outside file");
        let scope = FileScope::default();
        assert!(scope.allow("relative/folder").is_err());
        scope.allow(&shots).expect("allow");

        let url = AppUrl::parse(&file_src(&file)).expect("the URL parses");
        assert!(url.is_files());
        assert_eq!(
            scope.resolve(url.path()),
            Some(file.canonicalize().unwrap())
        );
        let secret = AppUrl::parse(&file_src(&root.join("secret.txt"))).expect("the URL parses");
        assert_eq!(scope.resolve(secret.path()), None);
        let folder = AppUrl::parse(&file_src(&shots.join("2026 11"))).expect("the URL parses");
        assert_eq!(scope.resolve(folder.path()), None);
        let climb = format!("{}/../../secret.txt", url.path());
        assert_eq!(scope.resolve(&climb), None);

        scope.revoke(&shots);
        assert_eq!(scope.resolve(url.path()), None);
        fs::remove_dir_all(&root).expect("cleanup");
    }
}
