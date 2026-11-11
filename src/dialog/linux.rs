use super::{FileDialog, MessageButtons, MessageDialog, MessageLevel, Pick};
use crate::dbus::{Bus, Message, SIGNAL, Value, unescape};
use ceri_engine::NativeHandle;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const CHOOSER: &str = "org.freedesktop.portal.FileChooser";
const MISSING: &str = "no dialog is available: install xdg-desktop-portal, zenity or kdialog";

static REQUESTS: AtomicU32 = AtomicU32::new(0);

pub(super) fn pick(dialog: &FileDialog, pick: Pick) -> Result<Option<Vec<PathBuf>>, String> {
    // a portal that is missing or too old leaves the choice to a helper program
    match portal(dialog, pick) {
        Ok(chosen) => Ok(chosen),
        Err(_) => helper(dialog, pick),
    }
}

fn bus_call(member: &str, body: Vec<Value>) -> Message {
    Message::call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        member,
        body,
    )
}

fn parent(owner: Option<NativeHandle>) -> String {
    match owner {
        Some(NativeHandle::Xlib { window }) => format!("x11:{window:x}"),
        Some(NativeHandle::Xcb { window }) => format!("x11:{window:x}"),
        _ => String::new(),
    }
}

// portal globs match case-sensitively, so each letter takes both cases
fn glob(extension: &str) -> String {
    if extension == "*" {
        return "*".to_string();
    }
    let letters: String = extension
        .chars()
        .map(|letter| match letter.is_alphabetic() {
            true => format!(
                "[{}{}]",
                letter.to_lowercase().collect::<String>(),
                letter.to_uppercase().collect::<String>()
            ),
            false => letter.to_string(),
        })
        .collect();
    format!("*.{letters}")
}

fn options(dialog: &FileDialog, pick: Pick, token: &str) -> Vec<(&'static str, Value)> {
    let (folder, name) = dialog.start();
    let mut options = vec![
        ("handle_token", Value::str(token)),
        ("modal", Value::Bool(true)),
    ];
    match pick {
        Pick::Files => options.push(("multiple", Value::Bool(dialog.multiple))),
        Pick::Folders => {
            options.push(("multiple", Value::Bool(dialog.multiple)));
            options.push(("directory", Value::Bool(true)));
        }
        Pick::Save => {
            if let Some(name) = name {
                options.push(("current_name", Value::str(name)));
            }
        }
    }
    if pick != Pick::Folders && !dialog.filters.is_empty() {
        let filters = dialog
            .filters
            .iter()
            .map(|filter| {
                let globs = filter
                    .extensions
                    .iter()
                    .map(|extension| {
                        Value::Struct(vec![Value::Uint32(0), Value::str(glob(extension))])
                    })
                    .collect();
                Value::Struct(vec![
                    Value::str(&filter.name),
                    Value::Array("(us)".into(), globs),
                ])
            })
            .collect();
        options.push(("filters", Value::Array("(sa(us))".into(), filters)));
    }
    if let Some(folder) = folder {
        let mut bytes = folder.as_os_str().as_bytes().to_vec();
        bytes.push(0);
        options.push(("current_folder", Value::Bytes(bytes)));
    }
    options
}

fn portal(dialog: &FileDialog, pick: Pick) -> Result<Option<Vec<PathBuf>>, String> {
    let (signals, received) = mpsc::channel();
    let bus = Bus::session(move |_, message| {
        if message.kind == SIGNAL {
            let _ = signals.send(message);
        }
    })?;
    let chosen = ask_portal(&bus, &received, dialog, pick);
    bus.close();
    chosen
}

fn ask_portal(
    bus: &Bus,
    received: &mpsc::Receiver<Message>,
    dialog: &FileDialog,
    pick: Pick,
) -> Result<Option<Vec<PathBuf>>, String> {
    let version = bus
        .call(Message::call(
            PORTAL,
            PORTAL_PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            vec![Value::str(CHOOSER), Value::str("version")],
        ))?
        .first()
        .and_then(Value::as_i64)
        .unwrap_or_default();
    // folders came in version 3
    if pick == Pick::Folders && version < 3 {
        return Err(format!("the portal file chooser is version {version}"));
    }
    for rule in [
        "type='signal',interface='org.freedesktop.portal.Request',member='Response'".to_string(),
        format!("type='signal',member='NameOwnerChanged',arg0='{PORTAL}'"),
    ] {
        bus.call(bus_call("AddMatch", vec![Value::Str(rule)]))?;
    }
    let token = format!(
        "ceri_{}_{}",
        std::process::id(),
        REQUESTS.fetch_add(1, Ordering::Relaxed)
    );
    let method = match pick {
        Pick::Save => "SaveFile",
        _ => "OpenFile",
    };
    let title = dialog.title.clone().unwrap_or_default();
    let reply = bus.call(Message::call(
        PORTAL,
        PORTAL_PATH,
        CHOOSER,
        method,
        vec![
            Value::str(parent(dialog.owner)),
            Value::str(title),
            Value::dict(options(dialog, pick, &token)),
        ],
    ))?;
    let request = reply
        .first()
        .and_then(Value::as_str)
        .ok_or("the portal answered without a request")?
        .to_string();
    // responses to other requests, and ones that came before the reply, pass by
    loop {
        let signal = match received.recv_timeout(Duration::from_secs(30)) {
            Ok(signal) => signal,
            // a quiet bus is asked whether it is still there
            Err(RecvTimeoutError::Timeout) => {
                bus.call(bus_call("GetId", Vec::new()))?;
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err("the session bus closed with the dialog open".to_string());
            }
        };
        match signal.member.as_deref() {
            Some("Response") if signal.path.as_deref() == Some(request.as_str()) => {
                let response = signal.body.first().and_then(Value::as_i64);
                let uris = signal.body.get(1).and_then(|results| results.get("uris"));
                return Ok(match (response, uris) {
                    (Some(0), Some(uris)) => {
                        Some(uris.items().iter().filter_map(file_path).collect())
                    }
                    _ => None,
                });
            }
            Some("NameOwnerChanged") => {
                return Err("the portal went away with the dialog open".to_string());
            }
            _ => {}
        }
    }
}

fn file_path(uri: &Value) -> Option<PathBuf> {
    let rest = uri.as_str()?.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    Some(PathBuf::from(OsString::from_vec(unescape(path))))
}

fn helper(dialog: &FileDialog, pick: Pick) -> Result<Option<Vec<PathBuf>>, String> {
    let (folder, name) = dialog.start();
    let start = match (folder, name) {
        (Some(folder), Some(name)) => folder.join(name).into_os_string(),
        // a trailing slash tells zenity the start is a folder
        (Some(folder), None) => {
            let mut folder = folder.into_os_string();
            folder.push("/");
            folder
        }
        (None, Some(name)) => name.into(),
        (None, None) => OsString::new(),
    };
    let mut zenity: Vec<OsString> = vec!["--file-selection".into()];
    // kdialog takes its options first, then the action and that action's own arguments
    let mut kdialog: Vec<OsString> = Vec::new();
    let mut action: Vec<OsString> = Vec::new();
    match pick {
        Pick::Files => action.push("--getopenfilename".into()),
        Pick::Folders => {
            zenity.push("--directory".into());
            action.push("--getexistingdirectory".into());
        }
        Pick::Save => {
            zenity.push("--save".into());
            action.push("--getsavefilename".into());
        }
    }
    action.push(start.clone());
    if !start.is_empty() {
        let mut filename = OsString::from("--filename=");
        filename.push(&start);
        zenity.push(filename);
    }
    if pick != Pick::Folders && !dialog.filters.is_empty() {
        let mut qt = Vec::new();
        for filter in &dialog.filters {
            let globs: Vec<_> = filter.extensions.iter().map(|e| glob(e)).collect();
            zenity.push(format!("--file-filter={} | {}", filter.name, globs.join(" ")).into());
            let patterns: Vec<_> = filter
                .extensions
                .iter()
                .map(|extension| match extension.as_str() {
                    "*" => "*".to_string(),
                    extension => format!("*.{extension}"),
                })
                .collect();
            qt.push(format!("{} ({})", filter.name, patterns.join(" ")));
        }
        action.push(qt.join("\n").into());
    }
    if dialog.multiple && pick != Pick::Save {
        zenity.extend(["--multiple".into(), "--separator=\n".into()]);
        kdialog.extend(["--multiple".into(), "--separate-output".into()]);
    }
    if let Some(title) = &dialog.title {
        zenity.push(format!("--title={title}").into());
        kdialog.extend(["--title".into(), title.into()]);
    }
    kdialog.extend(action);
    let output = run(vec![("zenity", zenity), ("kdialog", kdialog)])?;
    Ok(output.map(|stdout| {
        stdout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| PathBuf::from(OsString::from_vec(line.to_vec())))
            .collect()
    }))
}

// the first helper that is installed; none for a cancel, which both report with status 1
fn run(candidates: Vec<(&str, Vec<OsString>)>) -> Result<Option<Vec<u8>>, String> {
    for (program, arguments) in candidates {
        let output = match Command::new(program)
            .args(&arguments)
            .stdin(Stdio::null())
            .output()
        {
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("could not run {program}: {error}")),
            Ok(output) => output,
        };
        return match output.status.code() {
            Some(0) => Ok(Some(output.stdout)),
            Some(1) => Ok(None),
            _ => Err(format!(
                "{program} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )),
        };
    }
    Err(MISSING.to_string())
}

pub(super) fn message(dialog: &MessageDialog) -> Result<bool, String> {
    let (zenity_kind, kdialog_kind) = match (dialog.buttons, dialog.level) {
        (MessageButtons::Ok, MessageLevel::Info) => ("--info", "--msgbox"),
        (MessageButtons::Ok, MessageLevel::Warning) => ("--warning", "--sorry"),
        (MessageButtons::Ok, MessageLevel::Error) => ("--error", "--error"),
        (_, MessageLevel::Info) => ("--question", "--yesno"),
        (_, _) => ("--question", "--warningyesno"),
    };
    let mut zenity: Vec<OsString> = vec![
        zenity_kind.into(),
        "--no-markup".into(),
        format!("--title={}", dialog.title).into(),
        format!("--text={}", dialog.text).into(),
    ];
    let mut kdialog: Vec<OsString> = vec!["--title".into(), (&dialog.title).into()];
    if dialog.buttons == MessageButtons::OkCancel {
        zenity.extend(["--ok-label=OK".into(), "--cancel-label=Cancel".into()]);
        kdialog.extend([
            "--yes-label".into(),
            "OK".into(),
            "--no-label".into(),
            "Cancel".into(),
        ]);
    }
    kdialog.extend([kdialog_kind.into(), (&dialog.text).into()]);
    Ok(run(vec![("zenity", zenity), ("kdialog", kdialog)])?.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialog::FileFilter;

    #[test]
    fn portal_options_and_answers_follow_the_file_chooser_interface() {
        assert_eq!(glob("Mp4"), "*.[mM][pP]4");
        assert_eq!(glob("*"), "*");
        let dialog = FileDialog {
            default_path: Some(PathBuf::from("/")),
            filters: vec![FileFilter {
                name: "Video".into(),
                extensions: vec!["mp4".into()],
            }],
            multiple: true,
            ..FileDialog::default()
        };
        let folders = Value::dict(options(&dialog, Pick::Folders, "t"));
        assert_eq!(folders.get("directory"), Some(&Value::Bool(true)));
        assert_eq!(folders.get("filters"), None);
        assert_eq!(
            folders.get("current_folder"),
            Some(&Value::Bytes(b"/\0".to_vec()))
        );
        let files = Value::dict(options(&dialog, Pick::Files, "t"));
        assert_eq!(files.get("filters").unwrap().signature(), "a(sa(us))");
        assert_eq!(
            file_path(&Value::str("file:///home/me/clip%201.mp4")),
            Some(PathBuf::from("/home/me/clip 1.mp4"))
        );
        assert_eq!(
            file_path(&Value::str("file://host/srv/a")),
            Some(PathBuf::from("/srv/a"))
        );
        assert_eq!(file_path(&Value::str("https://example.com/a")), None);
        assert_eq!(parent(Some(NativeHandle::Xlib { window: 0x2a })), "x11:2a");
    }
}
