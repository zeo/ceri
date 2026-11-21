// native file and message dialogs: the shell's own on Windows; elsewhere the desktop portal's file
// chooser, with zenity or kdialog standing in where there is no portal and for messages

use ceri_engine::NativeHandle;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[cfg(target_os = "windows")]
#[path = "dialog/windows.rs"]
mod platform;

#[cfg(not(target_os = "windows"))]
#[path = "dialog/linux.rs"]
mod platform;

// extensions go without the dot; "*" takes any file
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct FileFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct FileDialog {
    pub title: Option<String>,
    // a folder to start in, or a file whose folder to start in and whose name to suggest
    pub default_path: Option<PathBuf>,
    pub filters: Vec<FileFilter>,
    pub multiple: bool,
    // the window the dialog belongs to, which waits on it
    pub owner: Option<NativeHandle>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pick {
    Files,
    Folders,
    Save,
}

impl FileDialog {
    // none when the user cancels
    pub fn open_files(&self) -> Result<Option<Vec<PathBuf>>, String> {
        platform::pick(self, Pick::Files)
    }

    pub fn open_folders(&self) -> Result<Option<Vec<PathBuf>>, String> {
        platform::pick(self, Pick::Folders)
    }

    pub fn save_file(&self) -> Result<Option<PathBuf>, String> {
        Ok(platform::pick(self, Pick::Save)?.and_then(|chosen| chosen.into_iter().next()))
    }

    // the folder to start in and the name to suggest
    fn start(&self) -> (Option<PathBuf>, Option<String>) {
        match &self.default_path {
            Some(path) if path.is_dir() => (Some(path.clone()), None),
            Some(path) => (
                path.parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .map(Path::to_path_buf),
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            ),
            None => (None, None),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageLevel {
    #[default]
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MessageButtons {
    #[default]
    Ok,
    OkCancel,
    YesNo,
}

#[derive(Clone, Debug, Default)]
pub struct MessageDialog {
    pub title: String,
    pub text: String,
    pub level: MessageLevel,
    pub buttons: MessageButtons,
    pub owner: Option<NativeHandle>,
    // words for the buttons in place of OK and Cancel, or Yes and No
    pub ok_label: Option<String>,
    pub cancel_label: Option<String>,
}

impl MessageDialog {
    // true for OK or Yes
    pub fn show(&self) -> Result<bool, String> {
        platform::message(self)
    }
}

// an input's accept list: its extensions as one filter, then any file; MIME types fall to the latter
pub(crate) fn accept_filters(accept: Option<&str>) -> Vec<FileFilter> {
    let extensions: Vec<String> = accept
        .unwrap_or_default()
        .split(',')
        .filter_map(|entry| entry.trim().strip_prefix('.'))
        .filter(|extension| !extension.is_empty())
        .map(str::to_string)
        .collect();
    let any = FileFilter {
        name: "All files".to_string(),
        extensions: vec!["*".to_string()],
    };
    match extensions.is_empty() {
        true => Vec::new(),
        false => vec![
            FileFilter {
                name: "Accepted files".to_string(),
                extensions,
            },
            any,
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_lists_become_filters_and_default_paths_split() {
        let filters = accept_filters(Some(".png, .JPG,image/*,"));
        assert_eq!(filters[0].extensions, ["png", "JPG"]);
        assert_eq!(filters[1].extensions, ["*"]);
        assert!(accept_filters(Some("image/*")).is_empty());
        assert!(accept_filters(None).is_empty());
        let folder = std::env::temp_dir();
        let dialog = |path: PathBuf| FileDialog {
            default_path: Some(path),
            ..FileDialog::default()
        };
        assert_eq!(dialog(folder.clone()).start(), (Some(folder.clone()), None));
        assert_eq!(
            dialog(folder.join("clip 1.mp4")).start(),
            (Some(folder), Some("clip 1.mp4".to_string()))
        );
        assert_eq!(
            dialog(PathBuf::from("name.txt")).start(),
            (None, Some("name.txt".to_string()))
        );
    }
}
