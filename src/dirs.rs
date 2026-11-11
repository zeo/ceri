// the per-user folders each desktop keeps for apps and for the user's own files
use std::path::PathBuf;

// settings that follow the user, roaming on windows
pub fn config() -> Option<PathBuf> {
    platform::config()
}

pub fn data() -> Option<PathBuf> {
    platform::data()
}

// data that stays on this machine
pub fn local_data() -> Option<PathBuf> {
    platform::local_data()
}

pub fn cache() -> Option<PathBuf> {
    platform::cache()
}

pub fn home() -> Option<PathBuf> {
    platform::home()
}

pub fn videos() -> Option<PathBuf> {
    platform::user(Folder::Videos)
}

pub fn documents() -> Option<PathBuf> {
    platform::user(Folder::Documents)
}

pub fn pictures() -> Option<PathBuf> {
    platform::user(Folder::Pictures)
}

pub fn music() -> Option<PathBuf> {
    platform::user(Folder::Music)
}

pub fn downloads() -> Option<PathBuf> {
    platform::user(Folder::Downloads)
}

pub fn desktop() -> Option<PathBuf> {
    platform::user(Folder::Desktop)
}

#[derive(Clone, Copy)]
enum Folder {
    Videos,
    Documents,
    Pictures,
    Music,
    Downloads,
    Desktop,
}

#[cfg(target_os = "windows")]
mod platform {
    use super::Folder;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_LocalAppData,
        FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_RoamingAppData,
        FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
    };
    use windows::core::GUID;

    // known folders follow the user's relocations, which environment variables do not
    fn known(id: &GUID) -> Option<PathBuf> {
        let path = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.ok()?;
        let wide = unsafe { path.as_wide() };
        let result = PathBuf::from(OsString::from_wide(wide));
        unsafe { CoTaskMemFree(Some(path.0 as _)) };
        Some(result)
    }

    pub fn config() -> Option<PathBuf> {
        known(&FOLDERID_RoamingAppData)
    }

    pub fn data() -> Option<PathBuf> {
        known(&FOLDERID_RoamingAppData)
    }

    pub fn local_data() -> Option<PathBuf> {
        known(&FOLDERID_LocalAppData)
    }

    pub fn cache() -> Option<PathBuf> {
        known(&FOLDERID_LocalAppData)
    }

    pub fn home() -> Option<PathBuf> {
        known(&FOLDERID_Profile)
    }

    pub fn user(folder: Folder) -> Option<PathBuf> {
        known(&match folder {
            Folder::Videos => FOLDERID_Videos,
            Folder::Documents => FOLDERID_Documents,
            Folder::Pictures => FOLDERID_Pictures,
            Folder::Music => FOLDERID_Music,
            Folder::Downloads => FOLDERID_Downloads,
            Folder::Desktop => FOLDERID_Desktop,
        })
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::Folder;
    use std::path::PathBuf;

    pub fn home() -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }

    pub fn config() -> Option<PathBuf> {
        home().map(|home| home.join("Library/Application Support"))
    }

    pub fn data() -> Option<PathBuf> {
        config()
    }

    pub fn local_data() -> Option<PathBuf> {
        config()
    }

    pub fn cache() -> Option<PathBuf> {
        home().map(|home| home.join("Library/Caches"))
    }

    pub fn user(folder: Folder) -> Option<PathBuf> {
        home().map(|home| {
            home.join(match folder {
                Folder::Videos => "Movies",
                Folder::Documents => "Documents",
                Folder::Pictures => "Pictures",
                Folder::Music => "Music",
                Folder::Downloads => "Downloads",
                Folder::Desktop => "Desktop",
            })
        })
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod platform {
    use super::Folder;
    use std::path::PathBuf;

    pub fn home() -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }

    // the spec ignores relative values
    fn base(variable: &str, fallback: &str) -> Option<PathBuf> {
        std::env::var_os(variable)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| home().map(|home| home.join(fallback)))
    }

    pub fn config() -> Option<PathBuf> {
        base("XDG_CONFIG_HOME", ".config")
    }

    pub fn data() -> Option<PathBuf> {
        base("XDG_DATA_HOME", ".local/share")
    }

    pub fn local_data() -> Option<PathBuf> {
        data()
    }

    pub fn cache() -> Option<PathBuf> {
        base("XDG_CACHE_HOME", ".cache")
    }

    // xdg-user-dirs names each folder in user-dirs.dirs; without one the defaults apply
    pub fn user(folder: Folder) -> Option<PathBuf> {
        let (key, fallback) = match folder {
            Folder::Videos => ("XDG_VIDEOS_DIR", "Videos"),
            Folder::Documents => ("XDG_DOCUMENTS_DIR", "Documents"),
            Folder::Pictures => ("XDG_PICTURES_DIR", "Pictures"),
            Folder::Music => ("XDG_MUSIC_DIR", "Music"),
            Folder::Downloads => ("XDG_DOWNLOAD_DIR", "Downloads"),
            Folder::Desktop => ("XDG_DESKTOP_DIR", "Desktop"),
        };
        let home = home()?;
        let listed = config()
            .and_then(|config| std::fs::read_to_string(config.join("user-dirs.dirs")).ok())
            .and_then(|text| super::user_dir(&text, key, &home));
        Some(listed.unwrap_or_else(|| home.join(fallback)))
    }
}

// reads one KEY="$HOME/path" or KEY="/abs/path" line of a user-dirs.dirs file
#[cfg_attr(any(target_os = "windows", target_os = "macos"), allow(dead_code))]
fn user_dir(text: &str, key: &str, home: &std::path::Path) -> Option<PathBuf> {
    let value = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| line.strip_prefix(key)?.trim_start().strip_prefix('='))?;
    let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
    let path = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None if value.starts_with('/') => PathBuf::from(value),
        None => return None,
    };
    // a folder set to home itself means the user turned it off
    (path != home).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::user_dir;
    use std::path::{Path, PathBuf};

    #[test]
    fn reads_user_dirs_lines() {
        let text = "# written by xdg-user-dirs-update\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_VIDEOS_DIR=\"/mnt/media/Videos\"\nXDG_MUSIC_DIR=\"$HOME/\"\nXDG_DOCUMENTS_DIR=\"Documents\"\n";
        let home = Path::new("/home/ana");
        assert_eq!(
            user_dir(text, "XDG_DESKTOP_DIR", home),
            Some(PathBuf::from("/home/ana/Desktop"))
        );
        assert_eq!(
            user_dir(text, "XDG_VIDEOS_DIR", home),
            Some(PathBuf::from("/mnt/media/Videos"))
        );
        assert_eq!(user_dir(text, "XDG_MUSIC_DIR", home), None);
        assert_eq!(user_dir(text, "XDG_DOCUMENTS_DIR", home), None);
        assert_eq!(user_dir(text, "XDG_PICTURES_DIR", home), None);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn finds_known_folders() {
        for path in [super::config(), super::local_data(), super::videos()] {
            assert!(path.is_some_and(|path| path.is_absolute()));
        }
    }
}
