// a per-user windows setup: the bundler's own executable, switched to the windows subsystem and
// given the app's icon, with the app folder packed after it. run, it installs to
// %LOCALAPPDATA%\Programs\<name> with a start menu entry and an entry under installed apps, then
// starts the app; /S installs without starting it or showing errors

use super::{Project, app_files};
use ceri_engine::{zlib_compress, zlib_decompress};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;
use windows::Win32::Foundation::{
    CloseHandle, DUPLICATE_HANDLE_OPTIONS, DuplicateHandle, ERROR_FILE_NOT_FOUND,
    ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, GENERIC_READ, HANDLE, WIN32_ERROR,
};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_DELETE, FILE_SHARE_READ, OPEN_EXISTING,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, IPersistFile,
};
use windows::Win32::System::LibraryLoader::{
    BeginUpdateResourceW, EndUpdateResourceW, UpdateResourceW,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD, REG_SZ, RRF_RT_REG_SZ, RegCloseKey,
    RegDeleteKeyValueW, RegDeleteTreeW, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
    RegSetKeyValueW,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, GetCurrentProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Programs, FOLDERID_UserProgramFiles, IShellLinkW, KF_FLAG_DEFAULT,
    SHGetKnownFolderPath, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{GUID, HSTRING, Interface, PCWSTR, PWSTR};

const MAGIC: &[u8; 8] = b"CERIPACK";
// the packed length, the unpacked length, then the magic
const TRAILER: usize = 24;
const UNINSTALLER: &str = "uninstall.exe";
// the uninstaller's copy in the temp folder takes this to finish the work its original cannot
const FINISH: &str = "--ceri-finish-uninstall";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
const EN_US: u16 = 0x0409;
const SUBSYSTEM_WINDOWS: u16 = 2;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Setup,
    Uninstall,
}

#[derive(Deserialize, Serialize)]
struct Header {
    kind: Kind,
    identifier: String,
    name: String,
    version: String,
    publisher: Option<String>,
    description: Option<String>,
    executable: String,
    files: Vec<(String, u64)>,
}

pub(super) fn write(
    project: &Project,
    app: &Path,
    output: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    let own = std::fs::read(std::env::current_exe()?)?;
    let mut stub = without_signature(&own, signature_start(&own).unwrap_or(own.len()));
    set_subsystem(&mut stub, SUBSYSTEM_WINDOWS).ok_or("the bundler is not a PE executable")?;
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    let setup = output.join(format!(
        "{}_{}_{arch}-setup.exe",
        project.name, project.version
    ));
    std::fs::write(&setup, &stub)?;

    let icon = project.icon("ico").map(std::fs::read).transpose()?;
    let description = format!("{} setup", project.name);
    let resources = ceri_build::Resources {
        product_name: Some(&project.name),
        description: Some(&description),
        company: project.publisher.as_deref(),
        ..Default::default()
    };
    let entries =
        ceri_build::resource_entries(&resources, icon.as_deref(), &project.name, &project.version);
    update_resources(&setup, &entries)
        .map_err(|error| format!("cannot give the setup its icon: {}", error.message()))?;
    let stub = std::fs::read(&setup)?;

    let header = Header {
        kind: Kind::Uninstall,
        identifier: project.identifier.clone(),
        name: project.name.clone(),
        version: project.version.clone(),
        publisher: project.publisher.clone(),
        description: project.description.clone(),
        executable: super::executable_name(&project.binary),
        files: Vec::new(),
    };
    // the uninstaller is the same stub, built and signed here so it ships signed like the setup
    let uninstaller = output.join(UNINSTALLER);
    std::fs::write(&uninstaller, [&stub[..], &pack(&header, &[])].concat())?;
    project.sign(&uninstaller)?;
    let uninstaller_bytes = std::fs::read(&uninstaller)?;
    std::fs::remove_file(&uninstaller)?;

    let mut files = Vec::new();
    let mut body = Vec::new();
    for (relative, path) in app_files(app)? {
        if relative.eq_ignore_ascii_case(UNINSTALLER) {
            return Err(
                format!("the app cannot ship a file of its own named {UNINSTALLER}").into(),
            );
        }
        let bytes = std::fs::read(path)?;
        files.push((relative, bytes.len() as u64));
        body.extend(bytes);
    }
    files.push((UNINSTALLER.to_owned(), uninstaller_bytes.len() as u64));
    body.extend(uninstaller_bytes);
    let header = Header {
        kind: Kind::Setup,
        files,
        ..header
    };
    std::fs::write(&setup, [&stub[..], &pack(&header, &body)].concat())?;
    project.sign(&setup)?;
    Ok(setup)
}

fn update_resources(file: &Path, entries: &[(u16, u16, Vec<u8>)]) -> windows::core::Result<()> {
    // the bundler's own resources go first, so a setup without an icon shows no ceri icon
    // SAFETY: the update handle is used only between its begin and end, each resource is named
    // by an integer id as MAKEINTRESOURCE does, and its data outlives the call that copies it
    unsafe {
        let update = BeginUpdateResourceW(&HSTRING::from(file.as_os_str()), true)?;
        for (kind, id, data) in entries {
            let written = UpdateResourceW(
                update,
                PCWSTR(usize::from(*kind) as *const u16),
                PCWSTR(usize::from(*id) as *const u16),
                EN_US,
                Some(data.as_ptr().cast()),
                data.len() as u32,
            );
            if let Err(error) = written {
                let _ = EndUpdateResourceW(update, true);
                return Err(error);
            }
        }
        EndUpdateResourceW(update, false)
    }
}

fn pack(header: &Header, body: &[u8]) -> Vec<u8> {
    let json = serde_json::to_vec(header).expect("the header is plain data");
    let mut payload = Vec::with_capacity(4 + json.len() + body.len());
    payload.extend((json.len() as u32).to_le_bytes());
    payload.extend(json);
    payload.extend_from_slice(body);
    let mut packed = zlib_compress(&payload);
    packed.extend((packed.len() as u64).to_le_bytes());
    packed.extend((payload.len() as u64).to_le_bytes());
    packed.extend(MAGIC);
    packed
}

// the header and files a setup or uninstaller carries, when it carries any
fn unpack(exe: &[u8]) -> Option<Result<(Header, Vec<u8>), String>> {
    let end = signature_start(exe).unwrap_or(exe.len());
    let trailer = exe.get(end.checked_sub(TRAILER)?..end)?;
    if trailer[16..] != MAGIC[..] {
        return None;
    }
    let word = |at: usize| u64::from_le_bytes(trailer[at..at + 8].try_into().unwrap()) as usize;
    let (packed, unpacked) = (word(0), word(8));
    let unpack = || {
        let start = (end - TRAILER)
            .checked_sub(packed)
            .ok_or("the setup is cut short")?;
        let mut payload = zlib_decompress(&exe[start..end - TRAILER], unpacked)
            .map_err(|error| error.to_string())?;
        let length = payload
            .get(..4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()) as usize)
            .ok_or("the setup has no header")?;
        let body = payload.split_off(4 + length.min(payload.len() - 4));
        let header = serde_json::from_slice(&payload[4..]).map_err(|error| error.to_string())?;
        Ok((header, body))
    };
    Some(unpack())
}

fn optional_header(exe: &[u8]) -> Option<usize> {
    let pe = u32::from_le_bytes(exe.get(0x3c..0x40)?.try_into().ok()?) as usize;
    (exe.get(pe..pe + 4)? == b"PE\0\0").then_some(pe + 24)
}

// where the security directory entry sits, which locates an authenticode signature
fn security_entry(exe: &[u8]) -> Option<usize> {
    let optional = optional_header(exe)?;
    let directories = match u16::from_le_bytes(exe.get(optional..optional + 2)?.try_into().ok()?) {
        0x10b => optional + 96,
        0x20b => optional + 112,
        _ => return None,
    };
    let entry = directories + 4 * 8;
    exe.get(entry..entry + 8).map(|_| entry)
}

// a signature sits after everything else in the file, so a signed setup's trailer ends there
fn signature_start(exe: &[u8]) -> Option<usize> {
    let entry = security_entry(exe)?;
    let offset = u32::from_le_bytes(exe[entry..entry + 4].try_into().unwrap()) as usize;
    let size = u32::from_le_bytes(exe[entry + 4..entry + 8].try_into().unwrap());
    (offset != 0 && size != 0 && offset <= exe.len()).then_some(offset)
}

fn without_signature(exe: &[u8], end: usize) -> Vec<u8> {
    let mut stub = exe[..end].to_vec();
    if let Some(entry) = security_entry(&stub) {
        stub[entry..entry + 8].fill(0);
    }
    stub
}

fn set_subsystem(exe: &mut [u8], subsystem: u16) -> Option<()> {
    let at = optional_header(exe)? + 68;
    exe.get_mut(at..at + 2)?
        .copy_from_slice(&subsystem.to_le_bytes());
    Some(())
}

pub(super) fn run_packed() -> Option<ExitCode> {
    let exe = std::fs::read(std::env::current_exe().ok()?).ok()?;
    let unpacked = unpack(&exe)?;
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    let silent = arguments
        .iter()
        .any(|argument| argument == "/S" || argument == "--silent");
    let finishing = arguments.first().is_some_and(|first| first == FINISH);
    let result = unpacked.and_then(|(header, body)| {
        match header.kind {
            Kind::Setup => install(&header, &body, silent),
            Kind::Uninstall if finishing => {
                let process = arguments
                    .get(1)
                    .and_then(|value| value.to_str()?.parse().ok());
                match (process, arguments.get(2)) {
                    (Some(process), Some(root)) => {
                        finish_uninstall(&header, process, Path::new(root))
                    }
                    _ => Err("the uninstaller was started without its folder".into()),
                }
            }
            Kind::Uninstall => begin_uninstall(&header, silent),
        }
        .map_err(|error| format!("{}: {error}", header.name))
    });
    let code = match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if !silent {
                // SAFETY: both strings outlive the call and there is no owner window
                unsafe {
                    MessageBoxW(
                        None,
                        &HSTRING::from(error.as_str()),
                        &HSTRING::from("Setup"),
                        MB_OK | MB_ICONERROR,
                    )
                };
            }
            ExitCode::FAILURE
        }
    };
    if finishing {
        // this copy's delete on close handle passes on once more, so it closes after this
        // process is gone and its image no longer pins the file, whether or not it succeeded
        let _ = Command::new("cmd.exe")
            .args(["/c", "exit"])
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn();
    }
    Some(code)
}

fn install(header: &Header, body: &[u8], silent: bool) -> Result<(), String> {
    let root = install_folder(header)?;
    let mut at = 0;
    let mut size = 0;
    for (relative, length) in &header.files {
        let length = *length as usize;
        let bytes = body.get(at..at + length).ok_or("the setup is cut short")?;
        at += length;
        size += length;
        let relative = Path::new(relative);
        if !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        {
            return Err(format!(
                "the setup holds a stray path {}",
                relative.display()
            ));
        }
        place(&root.join(relative), bytes)?;
    }

    let executable = root.join(&header.executable);
    let shortcut = known_folder(&FOLDERID_Programs)?.join(format!("{}.lnk", header.name));
    write_shortcut(&shortcut, &executable, &root, header.description.as_deref())
        .map_err(|error| format!("cannot add the start menu entry: {}", error.message()))?;
    register(header, &root, &executable, size)?;
    if !silent {
        Command::new(&executable)
            .current_dir(&root)
            .spawn()
            .map_err(|error| format!("installed, but cannot start: {error}"))?;
    }
    Ok(())
}

fn install_folder(header: &Header) -> Result<PathBuf, String> {
    Ok(known_folder(&FOLDERID_UserProgramFiles)?.join(&header.name))
}

// an uninstaller copied anywhere else would otherwise empty the folder it was copied to
fn check_install_folder(header: &Header, root: &Path) -> Result<(), String> {
    let expected = install_folder(header)?;
    match root.as_os_str().eq_ignore_ascii_case(expected.as_os_str()) {
        true => Ok(()),
        false => Err(format!(
            "{} is not where {} is installed",
            root.display(),
            header.name
        )),
    }
}

// a running copy of the app keeps its executable from being written, but not from being renamed
fn place(target: &Path, bytes: &[u8]) -> Result<(), String> {
    let failed = |error: std::io::Error| format!("cannot write {}: {error}", target.display());
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    if std::fs::write(target, bytes).is_ok() {
        return Ok(());
    }
    let mut aside = target.as_os_str().to_owned();
    aside.push(".old");
    let _ = std::fs::remove_file(&aside);
    std::fs::rename(target, &aside).map_err(failed)?;
    std::fs::write(target, bytes).map_err(failed)
}

fn register(header: &Header, root: &Path, executable: &Path, size: usize) -> Result<(), String> {
    let key = format!(r"{UNINSTALL_KEY}\{}", header.identifier);
    let uninstaller = root.join(UNINSTALLER);
    let strings = [
        ("DisplayName", Some(header.name.clone())),
        ("DisplayVersion", Some(header.version.clone())),
        ("Publisher", header.publisher.clone()),
        ("DisplayIcon", Some(executable.display().to_string())),
        ("InstallLocation", Some(root.display().to_string())),
        (
            "UninstallString",
            Some(format!("\"{}\"", uninstaller.display())),
        ),
        (
            "QuietUninstallString",
            Some(format!("\"{}\" /S", uninstaller.display())),
        ),
    ];
    for (name, value) in strings {
        if let Some(value) = value {
            let wide: Vec<u16> = value.encode_utf16().chain([0]).collect();
            set_value(&key, name, REG_SZ.0, bytes_of(&wide))?;
        }
    }
    let dwords = [
        ("NoModify", 1),
        ("NoRepair", 1),
        ("EstimatedSize", size.div_ceil(1024) as u32),
    ];
    for (name, value) in dwords {
        set_value(&key, name, REG_DWORD.0, &value.to_le_bytes())?;
    }
    Ok(())
}

fn bytes_of(wide: &[u16]) -> &[u8] {
    // SAFETY: a u16 slice is twice as many initialized bytes at the same address
    unsafe { std::slice::from_raw_parts(wide.as_ptr().cast(), wide.len() * 2) }
}

fn set_value(key: &str, name: &str, kind: u32, data: &[u8]) -> Result<(), String> {
    // SAFETY: the key and name are terminated wide strings and the size counts the data's bytes
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(name),
            kind,
            Some(data.as_ptr().cast()),
            data.len() as u32,
        )
    }
    .ok()
    .map_err(|error| format!("cannot register the app: {}", error.message()))
}

fn write_shortcut(
    shortcut: &Path,
    target: &Path,
    directory: &Path,
    description: Option<&str>,
) -> windows::core::Result<()> {
    // SAFETY: com is set up on this thread before the shell link is made, and every string
    // outlives the call it is passed to
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(target.as_os_str()))?;
        link.SetWorkingDirectory(&HSTRING::from(directory.as_os_str()))?;
        if let Some(description) = description {
            link.SetDescription(&HSTRING::from(description))?;
        }
        link.cast::<IPersistFile>()?
            .Save(&HSTRING::from(shortcut.as_os_str()), true)
    }
}

fn known_folder(id: &GUID) -> Result<PathBuf, String> {
    // SAFETY: the returned string is read, then freed with the allocator that made it
    unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None)
            .map_err(|error| format!("cannot find a known folder: {}", error.message()))?;
        let folder = PathBuf::from(OsString::from_wide(path.as_wide()));
        CoTaskMemFree(Some(path.0.cast()));
        Ok(folder)
    }
}

// a running executable cannot delete itself, so the installed uninstaller hands the work to a
// copy in the temp folder and exits; that copy is opened for deletion on close and the handle
// passes to it, so the file goes once nothing holds it anymore
fn begin_uninstall(header: &Header, silent: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let root = exe.parent().ok_or("the uninstaller has no folder")?;
    check_install_folder(header, root)?;
    let temp = std::env::temp_dir();
    let helper = temp.join(format!(
        "{}-uninstall-{}.exe",
        header.identifier,
        std::process::id()
    ));
    std::fs::copy(&exe, &helper).map_err(|error| error.to_string())?;
    let inherited = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    // SAFETY: the path is a terminated wide string and the attributes live through the call
    let file = unsafe {
        CreateFileW(
            &HSTRING::from(helper.as_os_str()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            Some(&inherited),
            OPEN_EXISTING,
            FILE_FLAG_DELETE_ON_CLOSE,
            None,
        )
    }
    .map_err(|error| error.message())?;
    let mut process = HANDLE::default();
    // SAFETY: the pseudo handle for this process is duplicated into a real, inheritable one
    let duplicated = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            GetCurrentProcess(),
            GetCurrentProcess(),
            &mut process,
            PROCESS_SYNCHRONIZE.0,
            true,
            DUPLICATE_HANDLE_OPTIONS(0),
        )
    };
    let spawned = duplicated.map_err(|error| error.message()).and_then(|()| {
        Command::new(&helper)
            .arg(FINISH)
            .arg((process.0 as usize).to_string())
            .arg(root)
            .args(silent.then_some("/S"))
            .current_dir(&temp)
            .spawn()
            .map(drop)
            .map_err(|error| error.to_string())
    });
    // SAFETY: both handles are this process's own, and the helper holds its inherited copies
    unsafe {
        let _ = CloseHandle(file);
        if !process.is_invalid() {
            let _ = CloseHandle(process);
        }
    }
    spawned
}

fn finish_uninstall(header: &Header, process: usize, root: &Path) -> Result<(), String> {
    // SAFETY: the handle was inherited from the uninstaller, which passed its value
    unsafe {
        let process = HANDLE(process as *mut _);
        WaitForSingleObject(process, 30_000);
        let _ = CloseHandle(process);
    }
    check_install_folder(header, root)?;
    let executable = root.join(&header.executable);
    // the app's executable goes first, so a running app stops the uninstall before anything is
    // removed
    retry(|| match std::fs::remove_file(&executable) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    })
    .map_err(|_| format!("close {} and uninstall it again", header.name))?;
    retry(|| std::fs::remove_dir_all(root))
        .map_err(|error| format!("cannot remove {}: {error}", root.display()))?;
    let shortcut = known_folder(&FOLDERID_Programs)?.join(format!("{}.lnk", header.name));
    let _ = std::fs::remove_file(shortcut);

    let identifier = HSTRING::from(header.identifier.as_str());
    // SAFETY: each key and value name is a terminated wide string
    unsafe {
        let _ = RegDeleteTreeW(
            HKEY_CURRENT_USER,
            &HSTRING::from(format!(r"{UNINSTALL_KEY}\{}", header.identifier)),
        );
        // what the runtime registers under the identifier: its notification app id and its
        // start at login value
        let _ = RegDeleteTreeW(
            HKEY_CURRENT_USER,
            &HSTRING::from(format!(
                r"Software\Classes\AppUserModelId\{}",
                header.identifier
            )),
        );
        let _ = RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(r"Software\Microsoft\Windows\CurrentVersion\Run"),
            &identifier,
        );
    }
    remove_schemes(&executable);
    Ok(())
}

fn retry<T>(mut attempt: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut tries = 0;
    loop {
        match attempt() {
            Err(_) if tries < 20 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            result => return result,
        }
    }
}

// the url schemes the runtime registered to open the app, found by the command they run
fn remove_schemes(executable: &Path) {
    let command = format!("\"{}\"", executable.display()).to_lowercase();
    let mut classes = HKEY::default();
    // SAFETY: the key is opened for reading, enumerated with a buffer of the longest key name,
    // and closed before the matching keys are deleted
    let schemes = unsafe {
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(r"Software\Classes"),
            None,
            KEY_READ,
            &mut classes,
        ) != ERROR_SUCCESS
        {
            return;
        }
        let mut names = Vec::new();
        let mut buffer = [0_u16; 256];
        for index in 0.. {
            let mut length = buffer.len() as u32;
            let status: WIN32_ERROR = RegEnumKeyExW(
                classes,
                index,
                Some(PWSTR(buffer.as_mut_ptr())),
                &mut length,
                None,
                None,
                None,
                None,
            );
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            if status == ERROR_SUCCESS && buffer[0] != u16::from(b'.') {
                names.push(String::from_utf16_lossy(&buffer[..length as usize]));
            }
        }
        let _ = RegCloseKey(classes);
        names
    };
    for scheme in schemes {
        let key = format!(r"Software\Classes\{scheme}");
        let opens = read_string(&format!(r"{key}\shell\open\command"))
            .is_some_and(|value| value.to_lowercase().starts_with(&command));
        if opens {
            // SAFETY: the key name is a terminated wide string
            let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key)) };
        }
    }
}

fn read_string(key: &str) -> Option<String> {
    let key = HSTRING::from(key);
    let mut size = 0_u32;
    // SAFETY: the first call only measures, the second fills a buffer of the measured size
    unsafe {
        let measured = RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        );
        if measured != ERROR_SUCCESS {
            return None;
        }
        let mut buffer = vec![0_u16; size.div_ceil(2) as usize];
        let read = RegGetValueW(
            HKEY_CURRENT_USER,
            &key,
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        );
        if read == ERROR_FILE_NOT_FOUND || read != ERROR_SUCCESS {
            return None;
        }
        let text = &buffer[..(size / 2) as usize];
        Some(String::from_utf16_lossy(
            text.strip_suffix(&[0]).unwrap_or(text),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // the smallest shape the PE helpers read: a dos stub pointing at the PE signature, then a
    // 64-bit optional header with its sixteen data directories
    fn image() -> Vec<u8> {
        let mut exe = vec![0; 0x200];
        exe[0x3c..0x40].copy_from_slice(&0x80_u32.to_le_bytes());
        exe[0x80..0x84].copy_from_slice(b"PE\0\0");
        exe[0x98..0x9a].copy_from_slice(&0x20b_u16.to_le_bytes());
        exe[0x98 + 68..0x98 + 70].copy_from_slice(&3_u16.to_le_bytes());
        exe
    }

    fn header(kind: Kind) -> Header {
        Header {
            kind,
            identifier: "lt.rot.clip".into(),
            name: "clip".into(),
            version: "0.1.4".into(),
            publisher: Some("zeo".into()),
            description: None,
            executable: "clip.exe".into(),
            files: vec![("clip.exe".into(), 3), ("assets/a.txt".into(), 2)],
        }
    }

    #[test]
    fn a_packed_setup_unpacks_its_header_and_files_after_the_stub() {
        let mut exe = image();
        assert!(unpack(&exe).is_none());
        set_subsystem(&mut exe, SUBSYSTEM_WINDOWS).unwrap();
        assert_eq!(exe[0x98 + 68..0x98 + 70], [2, 0]);
        exe.extend(pack(&header(Kind::Setup), b"exeab"));
        let (unpacked, body) = unpack(&exe).unwrap().unwrap();
        assert_eq!(unpacked.files, header(Kind::Setup).files);
        assert_eq!(body, b"exeab");
    }

    #[test]
    fn a_signature_after_the_trailer_is_skipped_and_dropped_from_the_stub() {
        let mut exe = image();
        exe.extend(pack(&header(Kind::Uninstall), &[]));
        let signed_at = exe.len();
        exe.extend([0xaa; 64]);
        let entry = security_entry(&exe).unwrap();
        exe[entry..entry + 4].copy_from_slice(&(signed_at as u32).to_le_bytes());
        exe[entry + 4..entry + 8].copy_from_slice(&64_u32.to_le_bytes());

        let (unpacked, _) = unpack(&exe).unwrap().unwrap();
        assert!(matches!(unpacked.kind, Kind::Uninstall));
        assert_eq!(signature_start(&exe), Some(signed_at));
        let stub = without_signature(&exe, signed_at);
        assert_eq!(stub.len(), signed_at);
        assert_eq!(signature_start(&stub), None);
    }
}
