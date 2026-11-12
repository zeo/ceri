// opens links, files and folders the way the desktop does: the shell's own handlers on Windows,
// xdg-open and the file manager's FileManager1 service elsewhere

use std::path::{Path, PathBuf};

// any URL the system has a handler for, mailto: and custom schemes included
pub fn open_url(url: &str) -> Result<(), String> {
    platform::open(url.as_ref())
}

// a file in the app that handles its type, or a folder in the file manager; an executable runs
pub fn open_path(path: impl AsRef<Path>) -> Result<(), String> {
    platform::open(existing(path.as_ref())?.as_os_str())
}

// the file manager on the folder holding the item, with the item selected
pub fn reveal(path: impl AsRef<Path>) -> Result<(), String> {
    platform::reveal(&existing(path.as_ref())?)
}

// checked first, so a missing item is an error here rather than a shell error box
fn existing(path: &Path) -> Result<PathBuf, String> {
    let path = std::path::absolute(path)
        .map_err(|error| format!("could not resolve {}: {error}", path.display()))?;
    match path.exists() {
        true => Ok(path),
        false => Err(format!("{} does not exist", path.display())),
    }
}

// what a page may open without the app's say: web, mail and phone links, as tauri's
// opener:allow-default-urls allows
pub(crate) fn page_may_open(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => rest.len() > 2 && rest.starts_with("//"),
        "mailto" | "tel" => true,
        _ => false,
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::ffi::OsStr;
    use std::path::Path;
    use std::thread;
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows::Win32::UI::Shell::{
        ILCreateFromPathW, ILFree, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, SHOpenFolderAndSelectItems,
        ShellExecuteExW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};

    // shell handlers may be com objects, so each call gets a single-threaded apartment of its own
    fn in_apartment(work: impl FnOnce() -> Result<(), String> + Send) -> Result<(), String> {
        thread::scope(|scope| {
            scope
                .spawn(|| {
                    // SAFETY: the apartment opens and closes on this thread
                    let opened = unsafe {
                        CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
                    }
                    .is_ok();
                    let result = work();
                    if opened {
                        // SAFETY: pairs the CoInitializeEx above
                        unsafe { CoUninitialize() };
                    }
                    result
                })
                .join()
                .unwrap_or_else(|_| Err("the shell call panicked".to_string()))
        })
    }

    pub(super) fn open(target: &OsStr) -> Result<(), String> {
        let file = HSTRING::from(target);
        in_apartment(|| {
            let mut execute = SHELLEXECUTEINFOW {
                cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
                // the thread ends after the call, so a handler that would start late runs first
                fMask: SEE_MASK_NOASYNC,
                lpVerb: w!("open"),
                lpFile: PCWSTR(file.as_ptr()),
                nShow: SW_SHOWNORMAL.0,
                ..Default::default()
            };
            // SAFETY: the info and the strings it points at live across the call
            unsafe { ShellExecuteExW(&mut execute) }.map_err(|error| {
                format!("could not open {}: {}", target.display(), error.message())
            })
        })
    }

    pub(super) fn reveal(path: &Path) -> Result<(), String> {
        let wide = HSTRING::from(path);
        in_apartment(|| {
            // SAFETY: the path string lives across the call
            let item = unsafe { ILCreateFromPathW(&wide) };
            if item.is_null() {
                return Err(format!("the shell could not find {}", path.display()));
            }
            // SAFETY: the item list is whole and freed once, after the call
            let shown = unsafe { SHOpenFolderAndSelectItems(item, None, 0) };
            unsafe { ILFree(Some(item)) };
            shown.map_err(|error| format!("could not show {}: {}", path.display(), error.message()))
        })
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use crate::dbus::{Bus, Message, Value};
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::process::{Command, Stdio};

    // xdg-open may wait on the app it starts, so it is left to finish on a thread of its own
    pub(super) fn open(target: &OsStr) -> Result<(), String> {
        let mut last = String::new();
        for program in ["xdg-open", "gio"] {
            let mut command = Command::new(program);
            if program == "gio" {
                command.arg("open");
            }
            let spawned = command
                .arg(target)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            match spawned {
                Ok(mut child) => {
                    std::thread::spawn(move || child.wait());
                    return Ok(());
                }
                Err(error) => last = format!("{program}: {error}"),
            }
        }
        Err(format!(
            "could not open {}: no xdg-open or gio ({last})",
            target.to_string_lossy()
        ))
    }

    // the file manager selects the item; without one on the bus its folder opens instead
    pub(super) fn reveal(path: &Path) -> Result<(), String> {
        let shown = Bus::session(|_, _| {}).and_then(|bus| {
            let reply = bus.call(Message::call(
                "org.freedesktop.FileManager1",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1",
                "ShowItems",
                vec![
                    Value::Array("s".to_string(), vec![Value::str(file_uri(path))]),
                    Value::str(""),
                ],
            ));
            bus.close();
            reply
        });
        match (shown, path.parent()) {
            (Ok(_), _) => Ok(()),
            (Err(_), Some(folder)) => open(folder.as_os_str()),
            (Err(error), None) => Err(error),
        }
    }

    pub(super) fn file_uri(path: &Path) -> String {
        let mut uri = String::from("file://");
        for &byte in path.as_os_str().as_bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                    uri.push(byte as char)
                }
                _ => uri.push_str(&format!("%{byte:02X}")),
            }
        }
        uri
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_open_only_web_mail_and_phone_links() {
        for url in [
            "https://example.com/a?b",
            "HTTP://example.com",
            "mailto:me@example.com",
            "tel:+15551234",
        ] {
            assert!(page_may_open(url), "{url}");
        }
        for url in [
            "file:///C:/Windows/System32/calc.exe",
            "C:\\Windows\\notepad.exe",
            "ms-settings:privacy",
            "https:",
            "javascript:alert(1)",
            " https://example.com",
            "",
        ] {
            assert!(!page_may_open(url), "{url}");
        }
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn file_uris_escape_everything_but_the_unreserved_bytes() {
        assert_eq!(
            platform::file_uri(Path::new("/home/me/clip 1#ü.mp4")),
            "file:///home/me/clip%201%23%C3%BC.mp4"
        );
    }

    #[test]
    fn a_missing_item_is_neither_opened_nor_revealed() {
        let missing = std::env::temp_dir().join("ceri-opener-missing-item");
        assert!(reveal(&missing).unwrap_err().contains("does not exist"));
        assert!(open_path(&missing).unwrap_err().contains("does not exist"));
    }
}
