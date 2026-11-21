// the commands an app registers for its pages to invoke, by name; payloads and results are
// JSON text

use crate::autostart::Autostart;
use crate::dialog::{FileDialog, FileFilter, MessageButtons, MessageDialog, MessageLevel};
use crate::notification::Notifier;
use crate::shortcut::GlobalShortcuts;
use crate::taskbar::{Progress, Taskbar};
use ceri_engine::{
    CommandCall, CommandError, CommandReply, CommandRouter, NativeHandle, WindowAction,
    WindowControl,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

type Handler = dyn Fn(&CommandCall) -> Result<String, CommandError> + Send + Sync;

#[derive(Clone)]
struct Registered {
    handler: Arc<Handler>,
    // runs on a thread of its own instead of the engine's
    blocking: bool,
}

#[derive(Clone, Default)]
pub struct Commands {
    registered: BTreeMap<String, Registered>,
}

impl Commands {
    // a handler quick enough to answer on the engine thread
    pub fn add<F>(&mut self, name: impl Into<String>, handler: F)
    where
        F: Fn(&CommandCall) -> Result<String, CommandError> + Send + Sync + 'static,
    {
        self.insert(name.into(), Arc::new(handler), false);
    }

    // a handler that may block, answered from a thread of its own
    pub fn add_blocking<F>(&mut self, name: impl Into<String>, handler: F)
    where
        F: Fn(&CommandCall) -> Result<String, CommandError> + Send + Sync + 'static,
    {
        self.insert(name.into(), Arc::new(handler), true);
    }

    fn insert(&mut self, name: String, handler: Arc<Handler>, blocking: bool) {
        self.registered
            .insert(name, Registered { handler, blocking });
    }
}

// routes a page's calls into the window's own commands and the app's
pub struct CommandRoutes {
    pub commands: Arc<Commands>,
    pub control: Option<WindowControl>,
    pub version: Option<String>,
    pub notifier: Arc<OnceLock<Notifier>>,
    pub taskbar: Arc<OnceLock<Taskbar>>,
    pub autostart: Autostart,
    pub shortcuts: GlobalShortcuts,
    pub opened_urls: Arc<Mutex<Vec<String>>>,
}

impl CommandRoutes {
    fn taskbar(&self) -> Result<&Taskbar, CommandError> {
        self.taskbar
            .get()
            .ok_or_else(|| CommandError::failed("the taskbar starts with the app"))
    }

    // commands every app has, for the page to drive its own window
    fn builtin(&self, call: &CommandCall) -> Option<Result<String, CommandError>> {
        if call.command == "app:version" {
            return Some(match &self.version {
                Some(version) => json(version),
                None => Err(CommandError::failed("the app has no version")),
            });
        }
        if call.command.starts_with("autostart:") {
            return Some(autostart(call, &self.autostart));
        }
        if call.command.starts_with("global-shortcut:") {
            return Some(global_shortcut(call, &self.shortcuts));
        }
        if call.command == "deep-link:get-current" {
            let urls = self
                .opened_urls
                .lock()
                .expect("opened urls poisoned")
                .clone();
            return Some(json(&Some(urls).filter(|urls| !urls.is_empty())));
        }
        let control = self.control.as_ref()?;
        let label = call.window.as_str();
        let act = |action| {
            control.act(label, action);
            Ok("null".to_string())
        };
        Some(match call.command.as_str() {
            "window:show" => act(WindowAction::Show),
            "window:hide" => act(WindowAction::Hide),
            "window:focus" => act(WindowAction::Focus),
            "window:minimize" => act(WindowAction::Minimize),
            "window:unminimize" => act(WindowAction::Unminimize),
            "window:maximize" => act(WindowAction::Maximize),
            "window:unmaximize" => act(WindowAction::Unmaximize),
            "window:toggle-maximize" => act(WindowAction::ToggleMaximize),
            "window:center" => act(WindowAction::Center),
            "window:request-attention" => act(WindowAction::RequestAttention),
            "window:close" => {
                control.close(label);
                Ok("null".to_string())
            }
            "window:set-fullscreen" => arguments::<Switch>(call)
                .and_then(|switch| act(WindowAction::Fullscreen(switch.on))),
            "window:set-always-on-top" => arguments::<Switch>(call)
                .and_then(|switch| act(WindowAction::AlwaysOnTop(switch.on))),
            "window:set-size" => arguments::<Extent>(call)
                .and_then(|size| act(WindowAction::Size(size.width, size.height))),
            "window:set-position" => {
                arguments::<Place>(call).and_then(|at| act(WindowAction::Position(at.x, at.y)))
            }
            "window:set-skip-taskbar" => arguments::<Switch>(call)
                .and_then(|switch| act(WindowAction::SkipTaskbar(switch.on))),
            "window:set-click-through" => arguments::<Switch>(call)
                .and_then(|switch| act(WindowAction::ClickThrough(switch.on))),
            "window:set-content-protected" => arguments::<Switch>(call)
                .and_then(|switch| act(WindowAction::ContentProtected(switch.on))),
            "window:labels" => json(&control.labels()),
            "window:monitors" => {
                let monitors: Vec<_> = control
                    .monitors()
                    .iter()
                    .map(|monitor| {
                        let ((x, y), (width, height)) = monitor.work_area;
                        serde_json::json!({
                            "name": monitor.name,
                            "primary": monitor.primary,
                            "scale": monitor.scale,
                            "position": { "x": monitor.position.0, "y": monitor.position.1 },
                            "size": { "width": monitor.size.0, "height": monitor.size.1 },
                            "workArea": { "x": x, "y": y, "width": width, "height": height },
                        })
                    })
                    .collect();
                json(&monitors)
            }
            "window:set-zoom" => {
                arguments::<Zoom>(call).and_then(|zoom| act(WindowAction::Zoom(zoom.zoom)))
            }
            "window:set-title" => {
                arguments::<Title>(call).and_then(|title| act(WindowAction::Title(title.title)))
            }
            "window:set-progress" => arguments::<ProgressRequest>(call).and_then(|request| {
                self.taskbar()?
                    .set_progress(control, label, request.into())
                    .map_err(CommandError::failed)?;
                Ok("null".to_string())
            }),
            "app:set-badge-count" => arguments::<BadgeRequest>(call).and_then(|request| {
                self.taskbar()?
                    .set_badge_count(control, request.count.filter(|count| *count > 0))
                    .map_err(CommandError::failed)?;
                Ok("null".to_string())
            }),
            "window:state" => match control.state(label) {
                Some(state) => json(&serde_json::json!({
                    "maximized": state.maximized,
                    "minimized": state.minimized,
                    "fullscreen": state.fullscreen,
                    "focused": state.focused,
                    "visible": state.visible,
                    "zoom": state.zoom,
                    "scale": state.scale,
                    "position": { "x": state.position.0, "y": state.position.1 },
                    "size": { "width": state.size.0, "height": state.size.1 },
                })),
                None => Err(CommandError::failed(format!(
                    "the {label} window is not open"
                ))),
            },
            _ => return None,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum ProgressState {
    None,
    Normal,
    Indeterminate,
    Paused,
    Error,
}

// electron's fraction from 0 to 1 and tauri's states; a progress alone is normal
#[derive(Deserialize)]
struct ProgressRequest {
    #[serde(default)]
    state: Option<ProgressState>,
    #[serde(default)]
    progress: Option<f64>,
}

impl From<ProgressRequest> for Progress {
    fn from(request: ProgressRequest) -> Self {
        let done = request.progress.unwrap_or(0.0);
        match (request.state, request.progress) {
            (None, None) | (Some(ProgressState::None), _) => Progress::None,
            (None | Some(ProgressState::Normal), _) => Progress::Normal(done),
            (Some(ProgressState::Indeterminate), _) => Progress::Indeterminate,
            (Some(ProgressState::Paused), _) => Progress::Paused(done),
            (Some(ProgressState::Error), _) => Progress::Error(done),
        }
    }
}

#[derive(Deserialize)]
struct BadgeRequest {
    count: Option<u64>,
}

#[derive(Deserialize)]
struct Switch {
    on: bool,
}

#[derive(Deserialize)]
struct Zoom {
    zoom: f32,
}

#[derive(Deserialize)]
struct Extent {
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
struct Place {
    x: i32,
    y: i32,
}

#[derive(Deserialize)]
struct Title {
    title: String,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct PickRequest {
    title: Option<String>,
    default_path: Option<PathBuf>,
    directory: bool,
    multiple: bool,
    filters: Vec<FileFilter>,
}

#[derive(Deserialize)]
struct MessageRequest {
    message: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    kind: MessageLevel,
}

// a dialog waits on the user, so it answers from a thread of its own; paths go back as strings,
// one or none, or a list when several were asked for
fn dialog(call: &CommandCall, owner: Option<NativeHandle>) -> Result<String, CommandError> {
    let command = call.command.as_str();
    let buttons = match command {
        "dialog:open" | "dialog:save" => {
            let request = arguments::<Option<PickRequest>>(call)?.unwrap_or_default();
            let multiple = request.multiple && command == "dialog:open";
            let dialog = FileDialog {
                title: request.title,
                default_path: request.default_path,
                filters: request.filters,
                multiple,
                owner,
            };
            let chosen = match (command, request.directory) {
                ("dialog:save", _) => dialog.save_file().map(|path| path.map(|path| vec![path])),
                (_, true) => dialog.open_folders(),
                (_, false) => dialog.open_files(),
            }
            .map_err(CommandError::failed)?;
            return match chosen {
                Some(paths) if !multiple => json(&paths.first()),
                chosen => json(&chosen),
            };
        }
        "dialog:message" => MessageButtons::Ok,
        "dialog:ask" => MessageButtons::YesNo,
        "dialog:confirm" => MessageButtons::OkCancel,
        _ => return Err(CommandError::unknown(command)),
    };
    let request = arguments::<MessageRequest>(call)?;
    let answer = MessageDialog {
        title: request.title.unwrap_or_default(),
        text: request.message,
        level: request.kind,
        buttons,
        owner,
    }
    .show()
    .map_err(CommandError::failed)?;
    json(&answer)
}

#[derive(Deserialize)]
struct OpenUrl {
    url: String,
}

#[derive(Deserialize)]
struct OpenPath {
    path: PathBuf,
}

// a page opens web, mail and phone links only; a path needs the app to grant the command
fn opener(call: &CommandCall) -> Result<String, CommandError> {
    match call.command.as_str() {
        "opener:open-url" => {
            let url = arguments::<OpenUrl>(call)?.url;
            if !crate::opener::page_may_open(&url) {
                return Err(CommandError::new(
                    "denied",
                    format!("a page may only open http, https, mailto and tel links, not {url}"),
                ));
            }
            crate::opener::open_url(&url)
        }
        "opener:open-path" => crate::opener::open_path(arguments::<OpenPath>(call)?.path),
        "opener:reveal" => crate::opener::reveal(arguments::<OpenPath>(call)?.path),
        command => return Err(CommandError::unknown(command)),
    }
    .map_err(CommandError::failed)?;
    Ok("null".to_string())
}

fn autostart(call: &CommandCall, autostart: &Autostart) -> Result<String, CommandError> {
    match call.command.as_str() {
        "autostart:enable" => autostart.enable().map(|()| "null".to_string()),
        "autostart:disable" => autostart.disable().map(|()| "null".to_string()),
        "autostart:is-enabled" => autostart.is_enabled().map(|on| on.to_string()),
        command => return Err(CommandError::unknown(command)),
    }
    .map_err(CommandError::failed)
}

#[derive(Deserialize)]
struct Accelerator {
    shortcut: String,
}

fn global_shortcut(
    call: &CommandCall,
    shortcuts: &GlobalShortcuts,
) -> Result<String, CommandError> {
    let shortcut = || arguments::<Accelerator>(call).map(|accelerator| accelerator.shortcut);
    match call.command.as_str() {
        "global-shortcut:register" => json(
            &shortcuts
                .register(&shortcut()?)
                .map_err(CommandError::failed)?,
        ),
        "global-shortcut:unregister" => {
            shortcuts
                .unregister(&shortcut()?)
                .map_err(CommandError::failed)?;
            Ok("null".to_string())
        }
        "global-shortcut:unregister-all" => {
            shortcuts.unregister_all();
            Ok("null".to_string())
        }
        "global-shortcut:is-registered" => json(
            &shortcuts
                .is_registered(&shortcut()?)
                .map_err(CommandError::failed)?,
        ),
        command => Err(CommandError::unknown(command)),
    }
}

#[derive(Deserialize)]
struct Notice {
    title: String,
    #[serde(default)]
    body: String,
}

// pages have no permission prompt to answer, so asking always grants, as tauri's does on desktop
fn notification(call: &CommandCall, notifier: Option<&Notifier>) -> Result<String, CommandError> {
    match call.command.as_str() {
        "notification:show" => {
            let notice = arguments::<Notice>(call)?;
            notifier
                .ok_or_else(|| CommandError::failed("notifications start with the app"))?
                .show(&notice.title, &notice.body)
                .map_err(CommandError::failed)?;
            Ok("null".to_string())
        }
        "notification:permission" => json(&"granted"),
        command => Err(CommandError::unknown(command)),
    }
}

fn arguments<T: DeserializeOwned>(call: &CommandCall) -> Result<T, CommandError> {
    serde_json::from_str(&call.payload)
        .map_err(|error| CommandError::new("invalid-payload", error.to_string()))
}

fn json(value: &impl Serialize) -> Result<String, CommandError> {
    serde_json::to_string(value).map_err(|error| CommandError::failed(error.to_string()))
}

// the named arguments of a call made through ceri::command!, from the object a page passes
pub struct Arguments(serde_json::Map<String, serde_json::Value>);

impl Arguments {
    pub fn of(call: &CommandCall) -> Result<Self, CommandError> {
        match serde_json::from_str(&call.payload) {
            Ok(serde_json::Value::Object(arguments)) => Ok(Self(arguments)),
            Ok(serde_json::Value::Null) => Ok(Self(serde_json::Map::new())),
            Ok(_) => Err(CommandError::new(
                "invalid-payload",
                "the arguments are not an object",
            )),
            Err(error) => Err(CommandError::new("invalid-payload", error.to_string())),
        }
    }

    // a Rust name read under its camelCase key; one left out reads as null, so an Option is none
    pub fn take<T: DeserializeOwned>(&mut self, name: &str) -> Result<T, CommandError> {
        let mut parts = name.split('_').filter(|part| !part.is_empty());
        let mut key = parts.next().unwrap_or_default().to_string();
        for part in parts {
            let mut letters = part.chars();
            key.extend(letters.next().map(|first| first.to_ascii_uppercase()));
            key.push_str(letters.as_str());
        }
        let value = self.0.remove(&key).unwrap_or_default();
        serde_json::from_value(value)
            .map_err(|error| CommandError::new("invalid-payload", format!("{key}: {error}")))
    }
}

// what a ceri::command! body evaluates to; a Result answers with its value or its error, and
// anything else serializable answers with itself
pub struct Answer<T>(pub T);

pub trait AnswerResult {
    fn answer(self) -> Result<String, CommandError>;
}

impl<R: Serialize, E: Into<CommandError>> AnswerResult for Answer<Result<R, E>> {
    fn answer(self) -> Result<String, CommandError> {
        json(&self.0.map_err(Into::into)?)
    }
}

pub trait AnswerValue {
    fn answer(self) -> Result<String, CommandError>;
}

// taken by reference, so method lookup tries a Result's own answer first
impl<T: Serialize> AnswerValue for &Answer<T> {
    fn answer(self) -> Result<String, CommandError> {
        json(&self.0)
    }
}

impl CommandRouter for CommandRoutes {
    fn invoke(&self, call: CommandCall, reply: CommandReply) {
        if let Some(answer) = self.builtin(&call) {
            reply.send(answer);
            return;
        }
        if call.command.starts_with("dialog:")
            || call.command.starts_with("opener:")
            || call.command.starts_with("notification:")
        {
            let command = call.command.clone();
            let notifier = self.notifier.clone();
            let owner = self
                .control
                .as_ref()
                .and_then(|control| control.state(&call.window))
                .and_then(|state| state.handle);
            let spawned = thread::Builder::new()
                .name(format!("ceri {}", call.command))
                .spawn(move || {
                    reply.send(match call.command.split(':').next() {
                        Some("opener") => opener(&call),
                        Some("notification") => notification(&call, notifier.get()),
                        _ => dialog(&call, owner),
                    })
                });
            if let Err(error) = spawned {
                eprintln!("ceri: could not start a thread for {command}: {error}");
            }
            return;
        }
        let Some(registered) = self.commands.registered.get(&call.command).cloned() else {
            reply.send(Err(CommandError::unknown(&call.command)));
            return;
        };
        if !registered.blocking {
            reply.send((registered.handler)(&call));
            return;
        }
        let spawned = thread::Builder::new()
            .name(format!("ceri command {}", call.command))
            .spawn(move || reply.send((registered.handler)(&call)));
        if let Err(error) = spawned {
            eprintln!("ceri: could not start a command thread: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    fn call(command: &str) -> CommandCall {
        CommandCall {
            window: "main".to_string(),
            origin: ceri_engine::APP_ORIGIN.to_string(),
            command: command.to_string(),
            payload: "{\"n\":2}".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn a_progress_alone_is_normal_and_a_state_alone_keeps_its_own() {
        let progress = |payload: &str| {
            Progress::from(serde_json::from_str::<ProgressRequest>(payload).expect("request"))
        };
        assert_eq!(progress("{}"), Progress::None);
        assert_eq!(progress(r#"{"progress":0.5}"#), Progress::Normal(0.5));
        assert_eq!(
            progress(r#"{"state":"none","progress":0.5}"#),
            Progress::None
        );
        assert_eq!(
            progress(r#"{"state":"indeterminate"}"#),
            Progress::Indeterminate
        );
        assert_eq!(
            progress(r#"{"state":"paused","progress":0.2}"#),
            Progress::Paused(0.2)
        );
        assert_eq!(progress(r#"{"state":"error"}"#), Progress::Error(0.0));
        assert!(serde_json::from_str::<ProgressRequest>(r#"{"state":"busy"}"#).is_err());
    }

    #[test]
    fn commands_answer_inline_from_threads_and_name_unknown_ones() {
        let mut commands = Commands::default();
        commands.add("echo", |call| Ok(call.payload.clone()));
        commands.add_blocking("thread", |_| {
            Ok(format!(
                "{:?}",
                thread::current().name().unwrap_or_default()
            ))
        });
        let routes = CommandRoutes {
            commands: Arc::new(commands),
            control: None,
            version: Some("1.2.3".to_string()),
            notifier: Arc::default(),
            taskbar: Arc::default(),
            autostart: Autostart::new("dev.ceri.router-test", "Router Test"),
            shortcuts: GlobalShortcuts::default(),
            opened_urls: Arc::new(Mutex::new(vec!["clip://open".to_string()])),
        };
        let (sender, receiver) = channel();
        let ask = |command: &str, id| {
            routes.invoke(call(command), CommandReply::new(id, sender.clone()));
            receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("the command answers")
        };
        assert_eq!(ask("echo", 1), (1, Ok("{\"n\":2}".to_string())));
        assert_eq!(
            ask("thread", 2),
            (2, Ok("\"ceri command thread\"".to_string()))
        );
        assert_eq!(
            ask("nothing", 3),
            (3, Err(CommandError::unknown("nothing")))
        );
        assert_eq!(ask("app:version", 4), (4, Ok("\"1.2.3\"".to_string())));
        assert_eq!(
            ask("deep-link:get-current", 40),
            (40, Ok("[\"clip://open\"]".to_string()))
        );
        assert_eq!(
            ask("window:minimize", 5),
            (5, Err(CommandError::unknown("window:minimize"))),
            "without a window loop the window commands are not there"
        );
        assert_eq!(
            ask("dialog:nothing", 6),
            (6, Err(CommandError::unknown("dialog:nothing")))
        );
        let (id, answer) = {
            let mut bad = call("dialog:open");
            bad.payload = "{\"multiple\":3}".to_string();
            routes.invoke(bad, CommandReply::new(7, sender.clone()));
            receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("answers")
        };
        assert_eq!(id, 7);
        assert_eq!(answer.expect_err("a bad payload").code, "invalid-payload");
        let mut program = call("opener:open-url");
        program.payload = r#"{"url":"file:///C:/Windows/System32/calc.exe"}"#.to_string();
        routes.invoke(program, CommandReply::new(8, sender.clone()));
        let (id, answer) = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("answers");
        assert_eq!(id, 8);
        assert_eq!(answer.expect_err("a file url").code, "denied");
        assert_eq!(
            ask("opener:nothing", 9),
            (9, Err(CommandError::unknown("opener:nothing")))
        );
        assert_eq!(
            ask("notification:permission", 10),
            (10, Ok("\"granted\"".to_string()))
        );
    }

    #[test]
    fn typed_commands_read_named_arguments_and_answer_values_or_results() {
        let with = |payload: &str| CommandCall {
            window: "main".to_string(),
            payload: payload.to_string(),
            ..Default::default()
        };
        let count = crate::command!(|paths: Vec<String>, mic_device: Option<String>| paths.len()
            + mic_device.map_or(0, |device| device.len()));
        assert_eq!(
            count(&with(r#"{"paths":["a","b"],"micDevice":"xyz"}"#)),
            Ok("5".to_string())
        );
        assert_eq!(count(&with(r#"{"paths":[]}"#)), Ok("0".to_string()));
        let wrong = count(&with(r#"{"paths":1}"#)).expect_err("paths is a list");
        assert_eq!(wrong.code, "invalid-payload");
        assert!(wrong.message.starts_with("paths: "), "{}", wrong.message);
        assert_eq!(
            count(&with("[]")).expect_err("not an object").code,
            "invalid-payload"
        );

        let refuse = crate::command!(|on: bool| if on { Ok(1) } else { Err("off".to_string()) });
        assert_eq!(refuse(&with(r#"{"on":true}"#)), Ok("1".to_string()));
        assert_eq!(
            refuse(&with(r#"{"on":false}"#)),
            Err(CommandError::failed("off"))
        );

        let window = crate::command!(|call| call.window.clone());
        assert_eq!(window(&with("{}")), Ok("\"main\"".to_string()));
        let both = crate::command!(|call, n: u8| format!("{} {n}", call.window));
        assert_eq!(both(&with(r#"{"n":2}"#)), Ok("\"main 2\"".to_string()));
        let nothing = crate::command!(|| Ok::<_, CommandError>(()));
        assert_eq!(nothing(&with("null")), Ok("null".to_string()));
    }
}
