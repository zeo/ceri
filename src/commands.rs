// the commands an app registers for its pages to invoke, by name; payloads and results are
// JSON text

use ceri_engine::{CommandCall, CommandError, CommandReply, CommandRouter};
use std::collections::BTreeMap;
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

// routes a page's calls into an app's commands
pub struct CommandRoutes(pub Arc<Commands>);

impl CommandRouter for CommandRoutes {
    fn invoke(&self, call: CommandCall, reply: CommandReply) {
        let Some(registered) = self.0.registered.get(&call.command).cloned() else {
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
        let routes = CommandRoutes(Arc::new(commands));
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
    }
}
