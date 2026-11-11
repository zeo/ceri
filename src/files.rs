use ceri_engine::FILES_ORIGIN;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

// folders the app lets its pages read, each under a name of its own on the files origin; a
// page sees the name and the path inside the folder, never where the folder is
#[derive(Clone, Default)]
pub struct FileGrants {
    folders: Arc<RwLock<BTreeMap<String, PathBuf>>>,
}

impl FileGrants {
    // a name already granted moves to the new folder
    pub fn grant(&self, name: &str, folder: impl Into<PathBuf>) -> Result<(), String> {
        let usable = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
        if !usable {
            return Err(format!(
                "the file grant name {name:?} must be letters, digits, hyphens and underscores"
            ));
        }
        let folder = folder.into();
        if !folder.is_absolute() {
            return Err(format!(
                "the {name} file grant needs an absolute folder, not {}",
                folder.display()
            ));
        }
        self.write().insert(name.to_string(), folder);
        Ok(())
    }

    pub fn revoke(&self, name: &str) {
        self.write().remove(name);
    }

    // the files URL of a path inside a granted folder, through the deepest grant that holds it
    pub fn url(&self, path: &Path) -> Option<String> {
        let folders = self.read();
        let (name, inner) = folders
            .iter()
            .filter_map(|(name, folder)| Some((name, inside(folder, path)?)))
            .min_by_key(|(_, inner)| inner.components().count())?;
        let mut url = format!("{FILES_ORIGIN}/{name}");
        for component in inner.components() {
            let Component::Normal(part) = component else {
                return None;
            };
            url.push('/');
            url.push_str(&encode(part.to_str()?));
        }
        Some(url)
    }

    // the file a files URL path names, when it exists inside its grant
    pub fn resolve(&self, url_path: &str) -> Option<PathBuf> {
        let (name, rest) = url_path
            .strip_prefix('/')?
            .split_once('/')
            .unwrap_or((url_path.strip_prefix('/')?, ""));
        let folder = self.read().get(name)?.clone();
        let mut path = folder.clone();
        for part in rest.split('/').filter(|part| !part.is_empty()) {
            if part == "." || part == ".." || part.contains(['\\', ':']) {
                return None;
            }
            path.push(part);
        }
        // links inside the folder may not lead out of it
        let real = path.canonicalize().ok()?;
        real.starts_with(folder.canonicalize().ok()?)
            .then_some(real)
            .filter(|real| real.is_file())
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, PathBuf>> {
        self.folders.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, BTreeMap<String, PathBuf>> {
        self.folders.write().unwrap_or_else(PoisonError::into_inner)
    }
}

// the part of path under folder, comparing the paths as given and then as the disk resolves them
fn inside(folder: &Path, path: &Path) -> Option<PathBuf> {
    if let Ok(inner) = path.strip_prefix(folder) {
        return Some(inner.to_path_buf());
    }
    let (folder, path) = (folder.canonicalize().ok()?, path.canonicalize().ok()?);
    path.strip_prefix(folder).ok().map(Path::to_path_buf)
}

fn encode(part: &str) -> String {
    let mut encoded = String::with_capacity(part.len());
    for byte in part.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::FileGrants;
    use ceri_engine::AppUrl;
    use std::fs;

    #[test]
    fn serves_granted_files_by_name_and_nothing_outside() {
        let root = std::env::temp_dir().join(format!("ceri-grants-{}", std::process::id()));
        let shots = root.join("shots");
        fs::create_dir_all(shots.join("2026 11")).expect("grant folder");
        fs::write(shots.join("2026 11").join("a#1.png"), b"png").expect("granted file");
        fs::write(root.join("secret.txt"), b"secret").expect("outside file");
        let grants = FileGrants::default();
        grants.grant("shots", &shots).expect("grant");
        assert!(grants.grant("../up", &shots).is_err());
        assert!(grants.grant("rel", "relative/folder").is_err());

        let file = shots.join("2026 11").join("a#1.png");
        let url = grants.url(&file).expect("a granted path has a URL");
        assert_eq!(url, "https://files.ceri.invalid/shots/2026%2011/a%231.png");
        assert_eq!(grants.url(&root.join("secret.txt")), None);

        let parsed = AppUrl::parse(&url).expect("the URL parses");
        assert_eq!(
            grants.resolve(parsed.path()),
            Some(file.canonicalize().unwrap())
        );
        assert_eq!(grants.resolve("/shots/../secret.txt"), None);
        assert_eq!(grants.resolve("/shots/2026 11"), None);
        assert_eq!(grants.resolve("/other/secret.txt"), None);
        assert_eq!(grants.resolve("/shots"), None);

        grants.revoke("shots");
        assert_eq!(grants.resolve(parsed.path()), None);
        fs::remove_dir_all(&root).expect("cleanup");
    }
}
