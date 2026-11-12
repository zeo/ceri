// custom url schemes that start the app: the current user's url protocol keys on Windows, an
// x-scheme-handler desktop entry and the user's mimeapps.list default elsewhere

use std::ffi::OsString;

// the schemes the browser, the shell and the web already mean something by
const TAKEN: [&str; 13] = [
    "http",
    "https",
    "file",
    "ftp",
    "mailto",
    "tel",
    "data",
    "blob",
    "javascript",
    "about",
    "ws",
    "wss",
    "ceri",
];

// rfc 3986 scheme syntax, two characters at least so a drive letter is never one
pub(crate) fn check_scheme(scheme: &str) -> Result<String, String> {
    let scheme = scheme.to_ascii_lowercase();
    let mut characters = scheme.chars();
    let usable = scheme.len() >= 2
        && characters.next().is_some_and(|c| c.is_ascii_alphabetic())
        && characters.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    match (usable, TAKEN.contains(&scheme.as_str())) {
        (true, false) => Ok(scheme),
        (true, true) => Err(format!("the {scheme} scheme is not the app's to take")),
        (false, _) => Err(format!(
            "the deep link scheme {scheme:?} must be a letter, then letters, digits, +, - and ."
        )),
    }
}

// the launch arguments that are links in one of the app's schemes
pub(crate) fn urls(schemes: &[String], arguments: &[OsString]) -> Vec<String> {
    arguments
        .iter()
        .filter_map(|argument| argument.to_str())
        .filter(|argument| {
            argument.split_once(':').is_some_and(|(scheme, _)| {
                schemes.iter().any(|own| own.eq_ignore_ascii_case(scheme))
            })
        })
        .map(str::to_string)
        .collect()
}

// points each scheme at this executable, writing only what differs from what is there
pub(crate) fn register(identifier: &str, name: &str, schemes: &[String]) -> Result<(), String> {
    platform::register(identifier, name, schemes, &crate::autostart::program()?)
}

#[cfg(target_os = "windows")]
mod platform {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegGetValueW, RegSetKeyValueW,
    };
    use windows::core::{HSTRING, PCWSTR, w};

    pub(super) fn key(scheme: &str) -> String {
        format!(r"Software\Classes\{scheme}")
    }

    pub(super) fn command(program: &Path) -> Vec<u16> {
        let mut line = OsString::from("\"");
        line.push(program);
        line.push("\" \"%1\"");
        line.encode_wide().chain([0]).collect()
    }

    pub(super) fn register(
        _identifier: &str,
        name: &str,
        schemes: &[String],
        program: &Path,
    ) -> Result<(), String> {
        let command = command(program);
        for scheme in schemes {
            let key = key(scheme);
            let open = format!(r"{key}\shell\open\command");
            if read(&open, PCWSTR::null())?.as_deref() == Some(&command[..command.len() - 1]) {
                continue;
            }
            let label: Vec<u16> = format!("URL:{name}").encode_utf16().chain([0]).collect();
            set(&key, PCWSTR::null(), &label)
                .and_then(|()| set(&key, w!("URL Protocol"), &[0]))
                .and_then(|()| set(&open, PCWSTR::null(), &command))
                .map_err(|error| format!("could not register the {scheme} scheme: {error}"))?;
        }
        Ok(())
    }

    fn set(key: &str, name: PCWSTR, value: &[u16]) -> Result<(), String> {
        // SAFETY: the value is a terminated wide string and the size counts its bytes
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(key),
                name,
                REG_SZ.0,
                Some(value.as_ptr().cast()),
                (value.len() * 2) as u32,
            )
        }
        .ok()
        .map_err(|error| error.message())
    }

    // the string without its terminator, or none when the key or value is missing
    pub(super) fn read(key: &str, name: PCWSTR) -> Result<Option<Vec<u16>>, String> {
        let key = HSTRING::from(key);
        let mut size = 0_u32;
        loop {
            let mut buffer = vec![0_u16; size.div_ceil(2) as usize];
            let pointer = match buffer.is_empty() {
                true => None,
                false => Some(buffer.as_mut_ptr().cast()),
            };
            // SAFETY: the buffer, when there is one, holds the size in bytes it is described by
            let read = unsafe {
                RegGetValueW(
                    HKEY_CURRENT_USER,
                    &key,
                    name,
                    RRF_RT_REG_SZ,
                    None,
                    pointer,
                    Some(&mut size),
                )
            };
            match read {
                ERROR_FILE_NOT_FOUND => return Ok(None),
                read if read.is_ok() && size == 0 => return Ok(Some(Vec::new())),
                read if read.is_ok() && pointer.is_some() => {
                    buffer.truncate((size / 2) as usize);
                    while buffer.last() == Some(&0) {
                        buffer.pop();
                    }
                    return Ok(Some(buffer));
                }
                // the first call only sizes the value, and it may grow between the two
                read if read.is_ok() || read == windows::Win32::Foundation::ERROR_MORE_DATA => {}
                other => {
                    return Err(format!(
                        "could not read the {} key: {}",
                        key,
                        other.to_hresult().message()
                    ));
                }
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use std::fs;
    use std::io::ErrorKind;
    use std::path::Path;
    use std::process::{Command, Stdio};

    pub(super) fn register(
        identifier: &str,
        name: &str,
        schemes: &[String],
        program: &Path,
    ) -> Result<(), String> {
        let applications = crate::dirs::data()
            .ok_or_else(|| "no data folder: HOME is not set".to_string())?
            .join("applications");
        let id = format!("{identifier}-handler.desktop");
        let entry = applications.join(&id);
        let desktop = desktop_entry(name, program, schemes);
        if fs::read_to_string(&entry).ok().as_deref() != Some(desktop.as_str()) {
            fs::create_dir_all(&applications)
                .and_then(|()| fs::write(&entry, &desktop))
                .map_err(|error| format!("could not write {}: {error}", entry.display()))?;
            refresh(&applications);
        }
        let list = crate::dirs::config()
            .ok_or_else(|| "no config folder: HOME is not set".to_string())?
            .join("mimeapps.list");
        // a list a dotfile manager links in stays a link
        let list = fs::canonicalize(&list).unwrap_or(list);
        let old = match fs::read_to_string(&list) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => String::new(),
            Err(error) => return Err(format!("could not read {}: {error}", list.display())),
        };
        let new = set_defaults(&old, &id, schemes);
        if new == old {
            return Ok(());
        }
        // written beside and moved over, so a crash never leaves the user's list cut short
        let draft = list.with_extension(format!("list.{}", std::process::id()));
        list.parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(&draft, &new))
            .and_then(|()| fs::rename(&draft, &list))
            .map_err(|error| {
                let _ = fs::remove_file(&draft);
                format!("could not write {}: {error}", list.display())
            })
    }

    pub(super) fn desktop_entry(name: &str, program: &Path, schemes: &[String]) -> String {
        let name: String = name
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let exec = crate::autostart::exec_argument(&program.to_string_lossy());
        let types: String = schemes
            .iter()
            .map(|scheme| format!("x-scheme-handler/{scheme};"))
            .collect();
        format!(
            "[Desktop Entry]\nType=Application\nName={name}\nExec={exec} %u\nTerminal=false\nNoDisplay=true\nMimeType={types}\n"
        )
    }

    // the mime cache some desktops look handlers up in; it is fine for it to be missing
    fn refresh(applications: &Path) {
        let spawned = Command::new("update-desktop-database")
            .arg(applications)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = spawned {
            std::thread::spawn(move || child.wait());
        }
    }

    // each scheme's default in the [Default Applications] group, the rest of the list as it was
    pub(super) fn set_defaults(list: &str, id: &str, schemes: &[String]) -> String {
        let mut lines: Vec<String> = list.lines().map(str::to_string).collect();
        let start = match lines
            .iter()
            .position(|line| line.trim() == "[Default Applications]")
        {
            Some(header) => header + 1,
            None => {
                if lines.last().is_some_and(|line| !line.trim().is_empty()) {
                    lines.push(String::new());
                }
                lines.push("[Default Applications]".to_string());
                lines.len()
            }
        };
        let mut end = lines[start..]
            .iter()
            .position(|line| line.trim_start().starts_with('['))
            .map_or(lines.len(), |next| start + next);
        let mut at = (start..end)
            .rev()
            .find(|&index| !lines[index].trim().is_empty())
            .map_or(start, |index| index + 1);
        for scheme in schemes {
            let key = format!("x-scheme-handler/{scheme}");
            let line = format!("{key}={id};");
            let existing = lines[start..end].iter().position(|line| {
                line.split_once('=')
                    .is_some_and(|(name, _)| name.trim() == key)
            });
            match existing {
                Some(index) => lines[start + index] = line,
                None => {
                    lines.insert(at, line);
                    at += 1;
                    end += 1;
                }
            }
        }
        let mut text = lines.join("\n");
        text.push('\n');
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn schemes_keep_to_rfc_3986_and_leave_the_web_its_own() {
        assert_eq!(check_scheme("Clip").unwrap(), "clip");
        assert_eq!(check_scheme("x-clip+v1.2").unwrap(), "x-clip+v1.2");
        for scheme in ["", "c", "1clip", "clip:", "cl ip", "clïp", "https", "File"] {
            assert!(check_scheme(scheme).is_err(), "{scheme}");
        }
    }

    #[test]
    fn only_links_in_the_apps_schemes_are_taken_from_a_launch() {
        let schemes = vec!["clip".to_string(), "x-clip".to_string()];
        let arguments: Vec<OsString> = [
            "--tray",
            "clip://open?id=1",
            "CLIP:share",
            "x-clip:a",
            "C:\\clip:x",
            "https://example.com",
            "clipper://no",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        assert_eq!(
            urls(&schemes, &arguments),
            ["clip://open?id=1", "CLIP:share", "x-clip:a"]
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn a_scheme_is_registered_to_open_this_executable() {
        use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteTreeW};
        use windows::core::{HSTRING, PCWSTR, w};
        let scheme = format!("ceri-test-{}", std::process::id());
        let program = Path::new(r"C:\Program Files\Clip\clip.exe");
        let registered = platform::register(
            "dev.ceri.test",
            "Ceri Test",
            std::slice::from_ref(&scheme),
            program,
        );
        let key = platform::key(&scheme);
        let read = |key: &str, name| {
            platform::read(key, name).map(|value| value.map(|v| String::from_utf16_lossy(&v)))
        };
        let label = read(&key, PCWSTR::null());
        let protocol = read(&key, w!("URL Protocol"));
        let command = read(&format!(r"{key}\shell\open\command"), PCWSTR::null());
        // SAFETY: removes only the key this test wrote
        unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key.as_str())) }
            .ok()
            .expect("the test key goes");
        registered.expect("registers");
        assert_eq!(label.unwrap().as_deref(), Some("URL:Ceri Test"));
        assert_eq!(protocol.unwrap().as_deref(), Some(""));
        assert_eq!(
            command.unwrap().as_deref(),
            Some(r#""C:\Program Files\Clip\clip.exe" "%1""#)
        );
        assert_eq!(platform::read(&key, PCWSTR::null()).unwrap(), None);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn the_handler_entry_takes_one_link_and_names_each_scheme() {
        let schemes = vec!["clip".to_string(), "x-clip".to_string()];
        assert_eq!(
            platform::desktop_entry("Clip\nApp", Path::new("/opt/clip app/clip"), &schemes),
            concat!(
                "[Desktop Entry]\nType=Application\nName=Clip App\n",
                "Exec=\"/opt/clip app/clip\" %u\nTerminal=false\nNoDisplay=true\n",
                "MimeType=x-scheme-handler/clip;x-scheme-handler/x-clip;\n"
            )
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn defaults_are_set_in_place_and_the_rest_of_the_list_kept() {
        let schemes = vec!["clip".to_string(), "x-clip".to_string()];
        let id = "dev.clip-handler.desktop";
        assert_eq!(
            platform::set_defaults("", id, &schemes),
            "[Default Applications]\nx-scheme-handler/clip=dev.clip-handler.desktop;\nx-scheme-handler/x-clip=dev.clip-handler.desktop;\n"
        );
        let list = concat!(
            "[Added Associations]\nx-scheme-handler/clip=old.desktop;\n\n",
            "[Default Applications]\ntext/html=firefox.desktop\nx-scheme-handler/clip = old.desktop;\n\n",
            "[Removed Associations]\nimage/png=gimp.desktop;"
        );
        assert_eq!(
            platform::set_defaults(list, id, &schemes),
            concat!(
                "[Added Associations]\nx-scheme-handler/clip=old.desktop;\n\n",
                "[Default Applications]\ntext/html=firefox.desktop\n",
                "x-scheme-handler/clip=dev.clip-handler.desktop;\n",
                "x-scheme-handler/x-clip=dev.clip-handler.desktop;\n\n",
                "[Removed Associations]\nimage/png=gimp.desktop;\n"
            )
        );
        let set = platform::set_defaults(list, id, &schemes);
        assert_eq!(platform::set_defaults(&set, id, &schemes), set);
    }
}
