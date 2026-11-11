// the commands an app registers for its pages to invoke, by name; payloads and results are
// JSON text

use crate::files::FileGrants;
use ceri_engine::{
    CommandCall, CommandError, CommandReply, CommandRouter, WindowAction, WindowControl,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
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
    pub files: FileGrants,
}

impl CommandRoutes {
    // commands every app has, for the page to drive its own window
    fn builtin(&self, call: &CommandCall) -> Option<Result<String, CommandError>> {
        if call.command == "app:version" {
            return Some(match &self.version {
                Some(version) => json(version),
                None => Err(CommandError::failed("the app has no version")),
            });
        }
        if call.command == "files:url" {
            return Some(arguments::<FilePath>(call).and_then(
                |file| match self.files.url(&file.path) {
                    Some(url) => json(&url),
                    None => Err(CommandError::failed("the path is outside every file grant")),
                },
            ));
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
            "window:set-zoom" => {
                arguments::<Zoom>(call).and_then(|zoom| act(WindowAction::Zoom(zoom.zoom)))
            }
            "window:set-title" => {
                arguments::<Title>(call).and_then(|title| act(WindowAction::Title(title.title)))
            }
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
struct FilePath {
    path: PathBuf,
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
struct Title {
    title: String,
}

fn arguments<T: DeserializeOwned>(call: &CommandCall) -> Result<T, CommandError> {
    serde_json::from_str(&call.payload)
        .map_err(|error| CommandError::new("invalid-payload", error.to_string()))
}

fn json(value: &impl Serialize) -> Result<String, CommandError> {
    serde_json::to_string(value).map_err(|error| CommandError::failed(error.to_string()))
}

impl CommandRouter for CommandRoutes {
    fn invoke(&self, call: CommandCall, reply: CommandReply) {
        if let Some(answer) = self.builtin(&call) {
            reply.send(answer);
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
        }
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
            files: FileGrants::default(),
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
            ask("window:minimize", 5),
            (5, Err(CommandError::unknown("window:minimize"))),
            "without a window loop the window commands are not there"
        );
    }
}
