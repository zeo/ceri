// one running copy per profile: a later launch hands its arguments and working directory to the
// first copy, over a named pipe on Windows and a unix socket elsewhere, then exits

use std::ffi::OsString;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) type LaunchHandler = Box<dyn Fn(Vec<OsString>, PathBuf) + Send + Sync>;

const MAX_LAUNCH: u64 = 1 << 20;
const FORWARD_TIMEOUT: Duration = Duration::from_secs(3);

// held for the life of the first copy; the lock goes with the process
pub(crate) struct Instance {
    _lock: File,
}

// none when a running copy took the launch and this one should exit
pub(crate) fn claim(
    profile: &Path,
    identifier: &str,
    arguments: &[OsString],
    on_launch: LaunchHandler,
) -> Result<Option<Instance>, String> {
    std::fs::create_dir_all(profile)
        .map_err(|error| format!("could not create {}: {error}", profile.display()))?;
    let lock_path = profile.join("instance.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|error| format!("could not open {}: {error}", lock_path.display()))?;
    let address = platform::address(profile, identifier);
    match lock.try_lock() {
        Ok(()) => {
            if let Err(error) = platform::listen(&address, on_launch) {
                eprintln!("ceri: later launches cannot reach this copy: {error}");
            }
            Ok(Some(Instance { _lock: lock }))
        }
        Err(TryLockError::WouldBlock) => {
            let directory = std::env::current_dir().unwrap_or_default();
            let launch = encode(&directory, arguments);
            let deadline = Instant::now() + FORWARD_TIMEOUT;
            loop {
                match platform::forward(&address, &launch) {
                    Ok(()) => return Ok(None),
                    Err(error) if Instant::now() >= deadline => {
                        return Err(format!("the running copy did not take the launch: {error}"));
                    }
                    // the first copy may hold the lock and not be listening yet
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        }
        Err(TryLockError::Error(error)) => {
            Err(format!("could not lock {}: {error}", lock_path.display()))
        }
    }
}

fn serve(stream: impl Read, on_launch: &LaunchHandler) {
    let mut launch = Vec::new();
    if stream.take(MAX_LAUNCH).read_to_end(&mut launch).is_err() {
        return;
    }
    if let Some((directory, arguments)) = decode(&launch) {
        on_launch(arguments, directory);
    }
}

// the directory then each argument, each as a little-endian length and its bytes
fn encode(directory: &Path, arguments: &[OsString]) -> Vec<u8> {
    let mut launch = Vec::new();
    for part in
        std::iter::once(directory.as_os_str()).chain(arguments.iter().map(|a| a.as_os_str()))
    {
        let bytes = platform::bytes(part);
        launch.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        launch.extend_from_slice(&bytes);
    }
    launch
}

fn decode(mut launch: &[u8]) -> Option<(PathBuf, Vec<OsString>)> {
    let mut parts = Vec::new();
    while !launch.is_empty() {
        let (length, rest) = launch.split_first_chunk::<4>()?;
        let length = u32::from_le_bytes(*length) as usize;
        let part = rest.get(..length)?;
        parts.push(platform::os_string(part)?);
        launch = &rest[length..];
    }
    let mut parts = parts.into_iter();
    Some((PathBuf::from(parts.next()?), parts.collect()))
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{LaunchHandler, serve};
    use std::ffi::{OsStr, OsString};
    use std::fs::{File, OpenOptions};
    use std::io::Write;
    use std::mem::ManuallyDrop;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::Path;
    use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND, SECURITY_ANONYMOUS,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId,
        PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
    use windows::core::HSTRING;

    // pipe names are machine wide, so the profile path's hash keeps each user's apart
    pub(super) fn address(profile: &Path, identifier: &str) -> String {
        let hash = profile
            .as_os_str()
            .encode_wide()
            .fold(0xcbf2_9ce4_8422_2325u64, |hash, unit| {
                (hash ^ u64::from(unit)).wrapping_mul(0x0100_0000_01b3)
            });
        format!(r"\\.\pipe\ceri-{identifier}-{hash:016x}")
    }

    pub(super) fn listen(address: &str, on_launch: LaunchHandler) -> Result<(), String> {
        // SAFETY: the name outlives the call, and the default security lets other users read only
        let pipe = unsafe {
            CreateNamedPipeW(
                &HSTRING::from(address),
                PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                64 * 1024,
                0,
                None,
            )
        };
        if pipe.is_invalid() {
            return Err(windows::core::Error::from_thread().message());
        }
        let pipe = pipe.0 as isize;
        std::thread::Builder::new()
            .name("ceri-instance".to_string())
            .spawn(move || {
                let pipe = HANDLE(pipe as *mut _);
                // the one instance is reused for every launch, so the name is never free to take
                loop {
                    // SAFETY: the pipe handle stays open for the life of the process
                    let connected = unsafe { ConnectNamedPipe(pipe, None) };
                    if let Err(error) = connected
                        && error.code() != ERROR_PIPE_CONNECTED.to_hresult()
                    {
                        eprintln!("ceri: stopped taking later launches: {error}");
                        return;
                    }
                    // SAFETY: as above; ManuallyDrop keeps the handle from being closed
                    let stream = ManuallyDrop::new(unsafe { File::from_raw_handle(pipe.0) });
                    serve(&*stream, &on_launch);
                    // SAFETY: as above
                    let _ = unsafe { DisconnectNamedPipe(pipe) };
                }
            })
            .map(drop)
            .map_err(|error| error.to_string())
    }

    pub(super) fn forward(address: &str, launch: &[u8]) -> Result<(), String> {
        // anonymous so a server squatting on the name cannot act as this user
        let mut pipe = OpenOptions::new()
            .write(true)
            .security_qos_flags(SECURITY_ANONYMOUS.0)
            .open(address)
            .map_err(|error| error.to_string())?;
        let mut server = 0;
        // SAFETY: the handle is the open pipe
        if unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut server) }.is_ok()
        {
            // this launch owns the foreground, so the running copy may raise its window
            let _ = unsafe { AllowSetForegroundWindow(server) };
        }
        pipe.write_all(launch).map_err(|error| error.to_string())
    }

    pub(super) fn bytes(part: &OsStr) -> Vec<u8> {
        part.encode_wide().flat_map(u16::to_le_bytes).collect()
    }

    pub(super) fn os_string(part: &[u8]) -> Option<OsString> {
        let (units, rest) = part.as_chunks::<2>();
        rest.is_empty().then(|| {
            OsString::from_wide(
                &units
                    .iter()
                    .map(|unit| u16::from_le_bytes(*unit))
                    .collect::<Vec<_>>(),
            )
        })
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::{LaunchHandler, serve};
    use std::ffi::{OsStr, OsString};
    use std::io::Write;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    // the runtime directory is private to its owner; the profile stands in when it is missing or
    // belongs to someone else, as WSLg's shared one can
    pub(super) fn address(profile: &Path, identifier: &str) -> PathBuf {
        let owner = |path: &Path| std::fs::metadata(path).map(|metadata| metadata.uid()).ok();
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|directory| {
                directory.is_absolute()
                    && owner(directory)
                        .is_some_and(|uid| Some(uid) == owner(Path::new("/proc/self")))
            })
            .map(|directory| directory.join(format!("ceri-{identifier}.sock")))
            .unwrap_or_else(|| profile.join("instance.sock"))
    }

    pub(super) fn listen(address: &Path, on_launch: LaunchHandler) -> Result<(), String> {
        // holding the lock means any socket left here belongs to a copy that has exited
        let _ = std::fs::remove_file(address);
        let listener = UnixListener::bind(address).map_err(|error| error.to_string())?;
        std::fs::set_permissions(address, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
        std::thread::Builder::new()
            .name("ceri-instance".to_string())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    // a launch that stalls must not hold up the ones after it
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    serve(stream, &on_launch);
                }
            })
            .map(drop)
            .map_err(|error| error.to_string())
    }

    pub(super) fn forward(address: &Path, launch: &[u8]) -> Result<(), String> {
        let mut stream = UnixStream::connect(address).map_err(|error| error.to_string())?;
        stream.write_all(launch).map_err(|error| error.to_string())
    }

    pub(super) fn bytes(part: &OsStr) -> Vec<u8> {
        part.as_bytes().to_vec()
    }

    pub(super) fn os_string(part: &[u8]) -> Option<OsString> {
        Some(OsString::from_vec(part.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn a_second_launch_reaches_the_first_copy_and_then_stands_down() {
        let profile = std::env::temp_dir().join(format!("ceri-instance-{}", std::process::id()));
        let identifier = format!("test-{}", std::process::id());
        let (sent, received) = mpsc::channel();
        let first = claim(
            &profile,
            &identifier,
            &[],
            Box::new(move |arguments, directory| sent.send((arguments, directory)).unwrap()),
        )
        .unwrap();
        assert!(first.is_some());
        let arguments = vec![OsString::from("--open"), OsString::from("clip ünï.mp4")];
        let second = claim(&profile, &identifier, &arguments, Box::new(|_, _| ())).unwrap();
        assert!(second.is_none());
        let (forwarded, directory) = received.recv_timeout(FORWARD_TIMEOUT).unwrap();
        assert_eq!(forwarded, arguments);
        assert_eq!(directory, std::env::current_dir().unwrap());
        assert!(
            claim(&profile, &identifier, &[], Box::new(|_, _| ()))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            received.recv_timeout(FORWARD_TIMEOUT).unwrap().0,
            Vec::<OsString>::new()
        );
        drop(first);
        let _ = std::fs::remove_dir_all(&profile);
    }

    #[test]
    fn a_torn_launch_is_dropped() {
        let launch = encode(Path::new("/home"), &[OsString::from("a")]);
        assert!(decode(&launch).is_some());
        assert_eq!(decode(&launch[..launch.len() - 1]), None);
        assert_eq!(decode(&[]), None);
    }
}
