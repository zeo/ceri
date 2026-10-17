// commands a page invokes on its host, events passed between them, and the allowlist that
// decides which window and origin may call what

use std::sync::mpsc::Sender;

// a command a page invoked, with its arguments as JSON text
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandCall {
    pub window: String,
    pub origin: String,
    pub command: String,
    pub payload: String,
}

// why a command failed; the page sees the code and message on a CeriBridgeError
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandError {
    pub code: String,
    pub message: String,
}

impl CommandError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::new("failed", message)
    }

    pub fn unknown(command: &str) -> Self {
        Self::new("unknown-command", format!("no command named {command}"))
    }
}

// a call's id and its answer
pub type CommandResult = (u64, Result<String, CommandError>);

// answers one call with JSON text, at once or later from another thread
pub struct CommandReply {
    id: u64,
    sender: Sender<CommandResult>,
}

impl CommandReply {
    pub fn new(id: u64, sender: Sender<CommandResult>) -> Self {
        Self { id, sender }
    }

    pub fn send(self, result: Result<String, CommandError>) {
        // a page that navigated away no longer waits for the answer
        let _ = self.sender.send((self.id, result));
    }
}

pub trait CommandRouter {
    // a call the capabilities allowed
    fn invoke(&self, call: CommandCall, reply: CommandReply);

    // an event the page emitted for the host
    fn event(&self, _window: &str, _name: &str, _payload: &str) {}
}

// which commands one set of windows may invoke from one set of origins; a pattern is `*`, an
// exact name, or a prefix ending in `*`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub windows: Vec<String>,
    pub origins: Vec<String>,
    pub commands: Vec<String>,
}

// grants are additive and anything no grant names is denied
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    grants: Vec<Grant>,
}

impl Capabilities {
    pub fn new(grants: Vec<Grant>) -> Self {
        Self { grants }
    }

    pub fn permits(&self, window: &str, origin: &str, command: &str) -> bool {
        self.grants.iter().any(|grant| {
            matches_any(&grant.windows, window)
                && matches_any(&grant.origins, origin)
                && matches_any(&grant.commands, command)
        })
    }
}

fn matches_any(patterns: &[String], value: &str) -> bool {
    patterns.iter().any(|pattern| {
        pattern == "*"
            || pattern == value
            || pattern
                .strip_suffix('*')
                .is_some_and(|prefix| value.starts_with(prefix))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(windows: &[&str], origins: &[&str], commands: &[&str]) -> Grant {
        let owned = |values: &[&str]| values.iter().map(|value| value.to_string()).collect();
        Grant {
            windows: owned(windows),
            origins: owned(origins),
            commands: owned(commands),
        }
    }

    #[test]
    fn capabilities_deny_by_default_and_match_patterns() {
        let origin = "https://app.ceri.invalid";
        let capabilities = Capabilities::new(vec![
            grant(&["main"], &[origin], &["greet", "window:*"]),
            grant(&["*"], &[origin], &["ping"]),
        ]);
        assert!(capabilities.permits("main", origin, "greet"));
        assert!(capabilities.permits("main", origin, "window:minimize"));
        assert!(capabilities.permits("settings", origin, "ping"));
        assert!(!capabilities.permits("settings", origin, "greet"));
        assert!(!capabilities.permits("main", "https://example.com", "greet"));
        assert!(!capabilities.permits("main", origin, "windows"));
        assert!(!Capabilities::default().permits("main", origin, "ping"));
    }
}
