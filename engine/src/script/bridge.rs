// window.ceri: invoke() sends a command to the host and settles when the host replies,
// listen() hears events the host emits, emit() sends one the other way

use super::object::Property;
use super::operations::plain_object;
use super::*;
use crate::bridge::{
    Capabilities, CommandCall, CommandError, CommandReply, CommandResult, CommandRouter, Waker,
};
use std::sync::mpsc::{Receiver, Sender, channel};

type Request = (String, Option<Vec<u8>>, Vec<(String, String)>);

// what a page's window.ceri talks to
#[derive(Clone)]
pub struct HostBridge {
    pub(crate) router: Rc<dyn CommandRouter>,
    pub(crate) capabilities: Rc<Capabilities>,
    pub(crate) window: String,
}

impl HostBridge {
    pub fn new(
        router: Rc<dyn CommandRouter>,
        capabilities: Capabilities,
        window: impl Into<String>,
    ) -> Self {
        Self {
            router,
            capabilities: Rc::new(capabilities),
            window: window.into(),
        }
    }
}

pub(super) struct BridgeState {
    bridge: HostBridge,
    origin: String,
    waker: Option<Waker>,
    sender: Sender<CommandResult>,
    receiver: Receiver<CommandResult>,
    pending: HashMap<u64, PromiseHandle>,
    next_id: u64,
    listeners: Vec<(u64, String, Value)>,
    object: RefCell<Option<Value>>,
}

impl BridgeState {
    pub(super) fn permits(&self, command: &str) -> bool {
        self.bridge
            .capabilities
            .permits(&self.bridge.window, &self.origin, command)
    }

    // a call the engine makes for the page, answered through reply
    pub(super) fn send(&self, command: &str, payload: String, reply: CommandReply) {
        let call = CommandCall {
            window: self.bridge.window.clone(),
            origin: self.origin.clone(),
            command: command.to_string(),
            payload,
            body: None,
            headers: Vec::new(),
        };
        self.bridge
            .router
            .invoke(call, reply.with_waker(self.waker.clone()));
    }
}

impl ScriptRuntime {
    pub(crate) fn set_host_bridge(
        &mut self,
        bridge: HostBridge,
        origin: String,
        waker: Option<Waker>,
    ) {
        let (sender, receiver) = channel();
        self.bridge = Some(Box::new(BridgeState {
            bridge,
            origin,
            waker,
            sender,
            receiver,
            pending: HashMap::new(),
            next_id: 0,
            listeners: Vec::new(),
            object: RefCell::new(None),
        }));
    }

    // one object for the page's lifetime, so window.ceri === window.ceri
    pub(super) fn ceri_object(&self) -> Value {
        let Some(state) = self.bridge.as_ref() else {
            return Value::Undefined;
        };
        if let Some(object) = state.object.borrow().as_ref() {
            return object.clone();
        }
        let object = plain_object([
            ("invoke".to_string(), Value::Builtin(Builtin::CeriInvoke)),
            ("listen".to_string(), Value::Builtin(Builtin::CeriListen)),
            ("emit".to_string(), Value::Builtin(Builtin::CeriEmit)),
            (
                "windowLabel".to_string(),
                Value::String(state.bridge.window.clone().into()),
            ),
        ]);
        state.object.replace(Some(object.clone()));
        object
    }

    fn bridge_error(&self, error: &CommandError) -> Value {
        let value = self.make_error("Error", &error.message);
        if let Value::Object(object) = &value {
            let mut object = object.borrow_mut();
            object.define(
                "name".to_string(),
                Property::hidden(Value::String("CeriBridgeError".into())),
            );
            object.define(
                "code".to_string(),
                Property::data(Value::String(error.code.clone().into())),
            );
        }
        value
    }

    fn payload_text(&mut self, value: Option<&Value>) -> Result<String, ScriptError> {
        match value {
            None | Some(Value::Undefined) => Ok("{}".to_string()),
            Some(value) => self.json_text(value),
        }
    }

    // invoke(command, arguments, { headers }) as Tauri's invoke takes them
    fn invoke_request(&mut self, arguments: &[Value]) -> Result<Request, ScriptError> {
        let body = match arguments.get(1).map(Value::resolve) {
            Some(Value::Bytes(bytes)) => Some(bytes.borrow().clone()),
            Some(Value::TypedArray(array)) => Some(uint8_array_bytes(&array.borrow())),
            _ => None,
        };
        let payload = match body {
            Some(_) => "null".to_string(),
            None => self.payload_text(arguments.get(1))?,
        };
        let headers = match arguments.get(2).map(Value::resolve) {
            None | Some(Value::Undefined) => Value::Undefined,
            Some(options) => self.get_property(&options, "headers")?,
        };
        let headers = match headers {
            Value::Undefined => Vec::new(),
            headers => self
                .own_enumerable_entries(&headers)?
                .into_iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), value.string_value()))
                .collect(),
        };
        Ok((payload, body, headers))
    }

    pub(super) fn ceri_invoke(&mut self, arguments: &[Value]) -> Result<Value, ScriptError> {
        let command = arguments
            .first()
            .map(Value::string_value)
            .unwrap_or_default();
        let (payload, body, headers) = match self.invoke_request(arguments) {
            Ok(request) => request,
            Err(error) => {
                let thrown = self.take_thrown(error);
                let message = self.thrown_message(&thrown);
                let error = CommandError::new("invalid-payload", message);
                return Ok(Value::Promise(
                    self.rejected_promise_value(self.bridge_error(&error)),
                ));
            }
        };
        let Some(state) = self.bridge.as_mut() else {
            let error = CommandError::new("bridge-unavailable", "the host bridge is unavailable");
            return Ok(Value::Promise(
                self.rejected_promise_value(self.bridge_error(&error)),
            ));
        };
        if !state
            .bridge
            .capabilities
            .permits(&state.bridge.window, &state.origin, &command)
        {
            let error = CommandError::new(
                "denied",
                format!(
                    "{command} is not allowed for window {} at {}",
                    state.bridge.window, state.origin
                ),
            );
            return Ok(Value::Promise(
                self.rejected_promise_value(self.bridge_error(&error)),
            ));
        }
        state.next_id += 1;
        let id = state.next_id;
        let promise = Self::pending_promise();
        state.pending.insert(id, promise.clone());
        let call = CommandCall {
            window: state.bridge.window.clone(),
            origin: state.origin.clone(),
            command,
            payload,
            body,
            headers,
        };
        let reply = CommandReply::new(id, state.sender.clone()).with_waker(state.waker.clone());
        let router = state.bridge.router.clone();
        router.invoke(call, reply);
        self.settle_command_replies()?;
        Ok(Value::Promise(promise))
    }

    // settles the promises of commands the host has answered
    pub(super) fn settle_command_replies(&mut self) -> Result<usize, ScriptError> {
        let mut settled = 0;
        loop {
            let Some(state) = self.bridge.as_mut() else {
                return Ok(settled);
            };
            let Ok((id, result)) = state.receiver.try_recv() else {
                return Ok(settled);
            };
            let Some(promise) = state.pending.remove(&id) else {
                continue;
            };
            settled += 1;
            let outcome = match result {
                Ok(text) => match self.parse_json(&text) {
                    Ok(value) => PromiseOutcome::Fulfilled(value),
                    Err(error) => {
                        let thrown = self.take_thrown(error);
                        let message = self.thrown_message(&thrown);
                        PromiseOutcome::Rejected(
                            self.bridge_error(&CommandError::new("invalid-response", message)),
                        )
                    }
                },
                Err(error) => PromiseOutcome::Rejected(self.bridge_error(&error)),
            };
            self.settle_promise(&promise, outcome);
        }
    }

    pub(super) fn ceri_listen(&mut self, arguments: &[Value]) -> Result<Value, ScriptError> {
        let name = arguments
            .first()
            .map(Value::string_value)
            .unwrap_or_default();
        let callback = arguments.get(1).cloned().unwrap_or(Value::Undefined);
        if !is_callable(&callback) {
            return Err(self.type_error("ceri.listen requires a callback"));
        }
        let Some(state) = self.bridge.as_mut() else {
            let error = CommandError::new("bridge-unavailable", "the host bridge is unavailable");
            return Ok(Value::Promise(
                self.rejected_promise_value(self.bridge_error(&error)),
            ));
        };
        state.next_id += 1;
        let id = state.next_id;
        state.listeners.push((id, name, callback));
        Ok(Value::Promise(self.fulfilled_promise(Value::Builtin(
            Builtin::CeriUnlisten(id),
        ))))
    }

    pub(super) fn ceri_unlisten(&mut self, id: u64) {
        if let Some(state) = self.bridge.as_mut() {
            state.listeners.retain(|(listener, _, _)| *listener != id);
        }
    }

    pub(super) fn ceri_emit(&mut self, arguments: &[Value]) -> Result<Value, ScriptError> {
        let name = arguments
            .first()
            .map(Value::string_value)
            .unwrap_or_default();
        let payload = self.payload_text(arguments.get(1))?;
        let Some(state) = self.bridge.as_ref() else {
            return Ok(Value::Promise(self.fulfilled_promise(Value::Undefined)));
        };
        let router = state.bridge.router.clone();
        let window = state.bridge.window.clone();
        router.event(&window, &name, &payload);
        Ok(Value::Promise(self.fulfilled_promise(Value::Undefined)))
    }

    // an event from the host reaches the page's listeners for its name as { event, payload }
    pub fn dispatch_host_event(&mut self, name: &str, payload: &str) -> Result<usize, ScriptError> {
        let listeners = self
            .bridge
            .as_ref()
            .map(|state| {
                state
                    .listeners
                    .iter()
                    .filter(|(_, listened, _)| listened == name)
                    .map(|(_, _, callback)| callback.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if listeners.is_empty() {
            return Ok(0);
        }
        let payload = self.parse_json(payload)?;
        let event = plain_object([
            ("event".to_string(), Value::String(name.to_string().into())),
            ("payload".to_string(), payload),
        ]);
        for listener in &listeners {
            if let Err(error) = self.call(listener.clone(), vec![event.clone()]) {
                let thrown = self.take_thrown(error);
                let message = self.thrown_message(&thrown);
                self.report(format!("uncaught error in a ceri listener: {message}"));
            }
        }
        self.drain_microtasks()?;
        Ok(listeners.len())
    }
}
