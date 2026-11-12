// starting the app when the user logs in: a value under the current user's Run key on Windows,
// an xdg autostart desktop entry elsewhere

use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Clone)]
pub struct Autostart {
    identifier: String,
    // the windows entry goes by the identifier alone
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    name: String,
    arguments: Vec<OsString>,
}

impl Autostart {
    // the identifier names the entry, the name is what a desktop shows for it
    pub fn new(identifier: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            identifier: identifier.into(),
            name: name.into(),
            arguments: Vec::new(),
        }
    }

    // what the app is started with at login
    pub fn arguments(mut self, arguments: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.arguments = arguments.into_iter().map(Into::into).collect();
        self
    }

    // points the entry at this executable, replacing one an older copy left
    pub fn enable(&self) -> Result<(), String> {
        platform::enable(self, &program()?)
    }

    pub fn disable(&self) -> Result<(), String> {
        platform::disable(self)
    }

    pub fn is_enabled(&self) -> Result<bool, String> {
        platform::is_enabled(self)
    }
}

// an AppImage runs from a mount that is gone after it exits, so the image itself is started
pub(crate) fn program() -> Result<PathBuf, String> {
    std::env::var_os("APPIMAGE")
        .filter(|_| cfg!(target_os = "linux"))
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map_or_else(std::env::current_exe, Ok)
        .map_err(|error| format!("could not find the app's executable: {error}"))
}

#[cfg(target_os = "windows")]
mod platform {
    use super::Autostart;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };
    use windows::core::{HSTRING, w};

    const RUN: windows::core::PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");

    pub(super) fn enable(autostart: &Autostart, program: &Path) -> Result<(), String> {
        let line = command_line(program, &autostart.arguments);
        // SAFETY: the line is terminated and the size counts its bytes
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN,
                &HSTRING::from(autostart.identifier.as_str()),
                REG_SZ.0,
                Some(line.as_ptr().cast()),
                (line.len() * 2) as u32,
            )
        }
        .ok()
        .map_err(|error| format!("could not start the app at login: {}", error.message()))
    }

    pub(super) fn disable(autostart: &Autostart) -> Result<(), String> {
        // SAFETY: the key and value names are terminated wide strings
        let removed = unsafe {
            RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                RUN,
                &HSTRING::from(autostart.identifier.as_str()),
            )
        };
        match removed {
            ERROR_FILE_NOT_FOUND => Ok(()),
            other => other.ok().map_err(|error| {
                format!(
                    "could not stop the app starting at login: {}",
                    error.message()
                )
            }),
        }
    }

    pub(super) fn is_enabled(autostart: &Autostart) -> Result<bool, String> {
        // SAFETY: no buffer is passed, so only the value's presence is read
        let read = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                RUN,
                &HSTRING::from(autostart.identifier.as_str()),
                RRF_RT_REG_SZ,
                None,
                None,
                None,
            )
        };
        match read {
            ERROR_FILE_NOT_FOUND => Ok(false),
            other => other
                .ok()
                .map(|()| true)
                .map_err(|error| format!("could not read the login entry: {}", error.message())),
        }
    }

    // quoted so CommandLineToArgvW and the C runtime split it back into the same arguments
    pub(super) fn command_line(program: &Path, arguments: &[std::ffi::OsString]) -> Vec<u16> {
        let mut line: Vec<u16> = Vec::new();
        line.push(b'"'.into());
        line.extend(program.as_os_str().encode_wide());
        line.push(b'"'.into());
        for argument in arguments {
            line.push(b' '.into());
            let wide: Vec<u16> = argument.encode_wide().collect();
            let plain = !wide.is_empty()
                && !wide
                    .iter()
                    .any(|&unit| matches!(unit, 0x20 | 0x09 | 0x0a | 0x0b | 0x22));
            if plain {
                line.extend(wide);
                continue;
            }
            line.push(b'"'.into());
            let mut backslashes = 0;
            for unit in wide {
                match unit {
                    0x5c => backslashes += 1,
                    0x22 => {
                        line.extend(std::iter::repeat_n(0x5c, backslashes * 2 + 1));
                        line.push(unit);
                        backslashes = 0;
                    }
                    _ => {
                        line.extend(std::iter::repeat_n(0x5c, backslashes));
                        line.push(unit);
                        backslashes = 0;
                    }
                }
            }
            line.extend(std::iter::repeat_n(0x5c, backslashes * 2));
            line.push(b'"'.into());
        }
        line.push(0);
        line
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::Autostart;
    use std::path::{Path, PathBuf};

    fn entry(autostart: &Autostart) -> Result<PathBuf, String> {
        let config =
            crate::dirs::config().ok_or_else(|| "no config folder: HOME is not set".to_string())?;
        Ok(config
            .join("autostart")
            .join(format!("{}.desktop", autostart.identifier)))
    }

    pub(super) fn enable(autostart: &Autostart, program: &Path) -> Result<(), String> {
        let entry = entry(autostart)?;
        let written = entry
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&entry, desktop_entry(autostart, program)));
        written.map_err(|error| format!("could not write {}: {error}", entry.display()))
    }

    pub(super) fn disable(autostart: &Autostart) -> Result<(), String> {
        let entry = entry(autostart)?;
        match std::fs::remove_file(&entry) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("could not remove {}: {error}", entry.display()))
            }
            _ => Ok(()),
        }
    }

    pub(super) fn is_enabled(autostart: &Autostart) -> Result<bool, String> {
        Ok(entry(autostart)?.is_file())
    }

    pub(super) fn desktop_entry(autostart: &Autostart, program: &Path) -> String {
        let mut exec = super::exec_argument(&program.to_string_lossy());
        for argument in &autostart.arguments {
            exec.push(' ');
            exec.push_str(&super::exec_argument(&argument.to_string_lossy()));
        }
        let name: String = autostart
            .name
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        format!(
            "[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\nX-GNOME-Autostart-enabled=true\n"
        )
    }
}

// the desktop entry spec's Exec quoting, then its string escape, which doubles every
// backslash again; % starts a field code, so a literal one is doubled
#[cfg(not(target_os = "windows"))]
pub(crate) fn exec_argument(argument: &str) -> String {
    let reserved = argument.is_empty()
        || argument
            .chars()
            .any(|c| c.is_whitespace() || "\"'\\><~|&;$*?#()`".contains(c) || c.is_control());
    let mut quoted = String::new();
    if reserved {
        quoted.push('"');
        for c in argument.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                quoted.push('\\');
            }
            quoted.push(c);
        }
        quoted.push('"');
    } else {
        quoted.push_str(argument);
    }
    quoted
        .replace('\\', "\\\\")
        .replace('%', "%%")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn the_run_line_splits_back_into_the_same_arguments() {
        use windows::Win32::Foundation::{HLOCAL, LocalFree};
        use windows::Win32::UI::Shell::CommandLineToArgvW;
        use windows::core::PCWSTR;
        let arguments = [
            "--tray",
            "",
            "two words",
            r#"say "hi""#,
            r"C:\trailing\",
            r#"back\"slash"#,
            r"a\\b",
        ];
        let program = std::path::Path::new(r"C:\Program Files\Clip\clip.exe");
        let line = platform::command_line(
            program,
            &arguments.iter().map(OsString::from).collect::<Vec<_>>(),
        );
        let mut count = 0;
        // SAFETY: the line is terminated and the list is freed once after it is read
        let split = unsafe {
            let list = CommandLineToArgvW(PCWSTR(line.as_ptr()), &mut count);
            let split: Vec<String> = (0..count as usize)
                .map(|index| (*list.add(index)).to_string().expect("utf-16"))
                .collect();
            LocalFree(Some(HLOCAL(list.cast())));
            split
        };
        let mut expected = vec![program.to_string_lossy().into_owned()];
        expected.extend(arguments.iter().map(|argument| argument.to_string()));
        assert_eq!(split, expected);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn the_desktop_entry_quotes_and_escapes_its_exec_line() {
        let autostart = Autostart::new("dev.ceri.app", "Ceri\nApp").arguments([
            "--tray",
            "50%",
            "two words",
            r"back\slash",
            "$HOME",
        ]);
        assert_eq!(
            platform::desktop_entry(&autostart, std::path::Path::new("/opt/ceri app/ceri")),
            concat!(
                "[Desktop Entry]\nType=Application\nName=Ceri App\n",
                r#"Exec="/opt/ceri app/ceri" --tray 50%% "two words" "back\\\\slash" "\\$HOME""#,
                "\nX-GNOME-Autostart-enabled=true\n"
            )
        );
    }
}
