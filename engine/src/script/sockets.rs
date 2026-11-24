// the WebSocket interface over the protocol threads in websocket.rs; what they report waits in
// a channel until the runtime is pumped, then fires as open, message, error and close events

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::realm::{Builder, Native};
use super::websocket::{self, Connection, Handshake, Outgoing, SocketEvent};
use super::*;
use std::sync::mpsc::{Receiver, Sender, channel};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SocketMember {
    Send,
    Close,
    Url,
    ReadyState,
    BufferedAmount,
    Extensions,
    Protocol,
    BinaryType,
    SetBinaryType,
}

const ACCESSORS: &[(&str, SocketMember, Option<SocketMember>)] = &[
    ("url", SocketMember::Url, None),
    ("readyState", SocketMember::ReadyState, None),
    ("bufferedAmount", SocketMember::BufferedAmount, None),
    ("extensions", SocketMember::Extensions, None),
    ("protocol", SocketMember::Protocol, None),
    (
        "binaryType",
        SocketMember::BinaryType,
        Some(SocketMember::SetBinaryType),
    ),
];

const STATES: &[(&str, f64)] = &[
    ("CONNECTING", 0.0),
    ("OPEN", 1.0),
    ("CLOSING", 2.0),
    ("CLOSED", 3.0),
];

const CONNECTING: u8 = 0;
const OPEN: u8 = 1;
const CLOSING: u8 = 2;
const CLOSED: u8 = 3;

pub(super) struct SocketInterfaces {
    prototype: ObjectHandle,
}

pub(super) fn create(
    build: &Builder,
    event_target: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> SocketInterfaces {
    let prototype = new_object(JsObject::with_prototype(
        Some(event_target.clone()),
        ObjectClass::Ordinary,
    ));
    for (name, member, length) in [
        ("send", SocketMember::Send, 1),
        ("close", SocketMember::Close, 0),
    ] {
        let function = build.function(Native::Socket(member), name, length);
        prototype.borrow_mut().define(
            name.to_string(),
            Property {
                enumerable: true,
                ..Property::data(Value::Object(function))
            },
        );
    }
    for &(name, getter, setter) in ACCESSORS {
        let get = Value::Object(build.function(Native::Socket(getter), &format!("get {name}"), 0));
        let set = setter.map(|setter| {
            Value::Object(build.function(Native::Socket(setter), &format!("set {name}"), 1))
        });
        prototype.borrow_mut().define(
            name.to_string(),
            Property {
                slot: Slot::accessor(Some(get), set),
                enumerable: true,
                writable: false,
                configurable: true,
            },
        );
    }
    prototype.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property::hidden(Value::String("WebSocket".into())),
    );
    let constructor = build.constructor(Native::SocketConstructor, "WebSocket", 1, &prototype);
    for &(name, value) in STATES {
        for target in [&constructor, &prototype] {
            target.borrow_mut().define(
                name.to_string(),
                Property {
                    slot: Slot::Data(Value::Number(value)),
                    enumerable: true,
                    writable: false,
                    configurable: false,
                },
            );
        }
    }
    globals.push(("WebSocket", Value::Object(constructor)));
    SocketInterfaces { prototype }
}

pub(super) struct SocketState {
    url: String,
    origin: String,
    ready_state: u8,
    buffered: u64,
    protocol: String,
    extensions: String,
    binary_as_buffer: bool,
    // dropped once the connection is over, which hangs up whatever is left of it
    connection: Option<Connection>,
}

pub(super) struct Sockets {
    pub(super) states: HashMap<u32, SocketState>,
    events: Sender<(u32, SocketEvent)>,
    reports: Receiver<(u32, SocketEvent)>,
    waker: Option<crate::bridge::Waker>,
}

impl Default for Sockets {
    fn default() -> Self {
        let (events, reports) = channel();
        Self {
            states: HashMap::default(),
            events,
            reports,
            waker: None,
        }
    }
}

// a subprotocol is an http token, RFC 9110 section 5.6.2
fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

impl ScriptRuntime {
    pub(super) fn socket_waker(&self) -> Option<crate::bridge::Waker> {
        self.sockets.waker.clone()
    }

    pub(crate) fn set_socket_waker(&mut self, waker: Option<crate::bridge::Waker>) {
        self.sockets.waker = waker;
    }

    fn socket_failure(&mut self, name: &str, message: &str) -> ScriptError {
        self.throw_value(dom_exception(name, message))
    }

    pub(super) fn construct_socket(
        &mut self,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let reference =
            self.coerce_string(arguments.first().cloned().unwrap_or(Value::Undefined))?;
        let base = self
            .base_url
            .as_ref()
            .and_then(|base| crate::weburl::Url::parse(&base.to_string(), None));
        let Some(mut url) = crate::weburl::Url::parse(&reference, base.as_ref()) else {
            return Err(self.socket_failure(
                "SyntaxError",
                &format!("Failed to construct 'WebSocket': The URL '{reference}' is invalid."),
            ));
        };
        match url.scheme() {
            "http" => url.set_protocol("ws"),
            "https" => url.set_protocol("wss"),
            "ws" | "wss" => {}
            scheme => {
                return Err(self.socket_failure(
                    "SyntaxError",
                    &format!(
                        "Failed to construct 'WebSocket': The URL's scheme must be either 'http', 'https', 'ws', or 'wss'. '{scheme}' is not allowed."
                    ),
                ));
            }
        }
        let href = url.to_string();
        // a serialized URL holds a number sign only where its fragment starts
        if href.contains('#') {
            return Err(self.socket_failure(
                "SyntaxError",
                &format!(
                    "Failed to construct 'WebSocket': The URL contains a fragment identifier ('{}'). Fragment identifiers are not allowed in WebSocket URLs.",
                    url.hash().trim_start_matches('#')
                ),
            ));
        }
        let protocols = match arguments.get(1).cloned().unwrap_or(Value::Undefined) {
            Value::Undefined => Vec::new(),
            value @ (Value::Object(_) | Value::Array(_)) => self
                .iterate_to_vec(value)?
                .into_iter()
                .map(|each| self.coerce_string(each))
                .collect::<Result<Vec<_>, _>>()?,
            value => vec![self.coerce_string(value)?],
        };
        for (index, protocol) in protocols.iter().enumerate() {
            let problem = if !is_token(protocol) {
                "invalid"
            } else if protocols[..index].contains(protocol) {
                "duplicated"
            } else {
                continue;
            };
            return Err(self.socket_failure(
                "SyntaxError",
                &format!(
                    "Failed to construct 'WebSocket': The subprotocol '{protocol}' is {problem}."
                ),
            ));
        }
        let prototype = match new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.sockets.prototype.clone(),
        };
        let object = self.new_event_target(prototype);
        let ObjectClass::EventTarget(id) = object.borrow().class else {
            unreachable!("new_event_target makes event targets")
        };
        // a socket that has not closed stays alive with its listeners, as in browsers
        self.held_targets.insert(id, object.clone());
        let check = self.content_security_policy().url(
            crate::csp::Directive::Connect,
            &href,
            crate::csp::Initiator::default(),
        );
        let connector = if self.permits(check) {
            self.resource_provider
                .as_ref()
                .and_then(|provider| provider.connect_socket(&href))
        } else {
            None
        };
        let connection = match connector {
            Some(connector) => {
                let handshake = Handshake {
                    target: format!("{}{}", url.pathname(), url.search()),
                    origin: crate::navigation::APP_ORIGIN.to_string(),
                    protocols,
                };
                Some(websocket::open(
                    connector,
                    handshake,
                    id,
                    self.sockets.events.clone(),
                    self.sockets.waker.clone(),
                ))
            }
            None => {
                self.report(format!("WebSocket connection to '{href}' failed"));
                let _ = self.sockets.events.send((id, websocket::failure()));
                if let Some(waker) = &self.sockets.waker {
                    waker();
                }
                None
            }
        };
        self.sockets.states.insert(
            id,
            SocketState {
                url: href,
                origin: url.origin(),
                ready_state: CONNECTING,
                buffered: 0,
                protocol: String::new(),
                extensions: String::new(),
                binary_as_buffer: false,
                connection,
            },
        );
        Ok(Value::Object(object))
    }

    fn this_socket(&mut self, this: &Value) -> Result<u32, ScriptError> {
        if let Value::Object(object) = this
            && let ObjectClass::EventTarget(id) = object.borrow().class
            && self.sockets.states.contains_key(&id)
        {
            return Ok(id);
        }
        Err(self.type_error("Illegal invocation"))
    }

    pub(super) fn call_socket(
        &mut self,
        member: SocketMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let id = self.this_socket(&this)?;
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        Ok(match member {
            SocketMember::Send => {
                if self.sockets.states[&id].ready_state == CONNECTING {
                    return Err(self.socket_failure(
                        "InvalidStateError",
                        "Failed to execute 'send' on 'WebSocket': Still in CONNECTING state.",
                    ));
                }
                let message = match first {
                    Value::Blob(blob) => Outgoing::Binary(blob.bytes.clone()),
                    value @ (Value::Bytes(_) | Value::TypedArray(_)) => {
                        Outgoing::Binary(self.buffer_source_bytes(&value)?)
                    }
                    Value::Object(ref object)
                        if matches!(object.borrow().class, ObjectClass::DataView(_)) =>
                    {
                        Outgoing::Binary(self.buffer_source_bytes(&first)?)
                    }
                    value => Outgoing::Text(self.coerce_string(value)?),
                };
                let length = match &message {
                    Outgoing::Text(text) => text.len(),
                    Outgoing::Binary(bytes) => bytes.len(),
                    _ => 0,
                };
                let socket = self.sockets.states.get_mut(&id).expect("checked");
                // a socket past open still counts what it was given, though it sends nothing
                socket.buffered += length as u64;
                if socket.ready_state == OPEN
                    && let Some(connection) = &socket.connection
                {
                    connection.send(message);
                }
                Value::Undefined
            }
            SocketMember::Close => {
                let code = match first {
                    Value::Undefined => None,
                    value => {
                        let code = self.coerce_number(value)?;
                        let code = if code.is_nan() {
                            0.0
                        } else {
                            code.clamp(0.0, 65535.0).round_ties_even()
                        };
                        if code != 1000.0 && !(3000.0..=4999.0).contains(&code) {
                            return Err(self.socket_failure(
                                "InvalidAccessError",
                                &format!(
                                    "Failed to execute 'close' on 'WebSocket': The close code must be either 1000, or between 3000 and 4999. {code} is neither."
                                ),
                            ));
                        }
                        Some(code as u16)
                    }
                };
                let reason = match arguments.get(1).cloned().unwrap_or(Value::Undefined) {
                    Value::Undefined => String::new(),
                    value => self.coerce_string(value)?,
                };
                if reason.len() > 123 {
                    return Err(self.socket_failure(
                        "SyntaxError",
                        "Failed to execute 'close' on 'WebSocket': The close reason must not be greater than 123 UTF-8 bytes.",
                    ));
                }
                let socket = self.sockets.states.get_mut(&id).expect("checked");
                match socket.ready_state {
                    CONNECTING => {
                        socket.ready_state = CLOSING;
                        if let Some(connection) = &socket.connection {
                            connection.abort();
                        }
                    }
                    OPEN => {
                        socket.ready_state = CLOSING;
                        if let Some(connection) = &socket.connection {
                            connection.send(Outgoing::Close(code, reason));
                        }
                    }
                    _ => {}
                }
                Value::Undefined
            }
            SocketMember::Url => Value::String(self.sockets.states[&id].url.clone().into()),
            SocketMember::ReadyState => {
                Value::Number(f64::from(self.sockets.states[&id].ready_state))
            }
            SocketMember::BufferedAmount => Value::Number(self.sockets.states[&id].buffered as f64),
            SocketMember::Extensions => {
                Value::String(self.sockets.states[&id].extensions.clone().into())
            }
            SocketMember::Protocol => {
                Value::String(self.sockets.states[&id].protocol.clone().into())
            }
            SocketMember::BinaryType => Value::String(
                if self.sockets.states[&id].binary_as_buffer {
                    "arraybuffer"
                } else {
                    "blob"
                }
                .into(),
            ),
            SocketMember::SetBinaryType => {
                let kind = self.coerce_string(first)?;
                if let "blob" | "arraybuffer" = kind.as_str() {
                    self.sockets
                        .states
                        .get_mut(&id)
                        .expect("checked")
                        .binary_as_buffer = kind == "arraybuffer";
                }
                Value::Undefined
            }
        })
    }

    pub(super) fn pump_sockets(&mut self) -> Result<usize, ScriptError> {
        let reports = self.sockets.reports.try_iter().collect::<Vec<_>>();
        let processed = reports.len();
        for (id, report) in reports {
            let Some(socket) = self.sockets.states.get_mut(&id) else {
                continue;
            };
            match report {
                SocketEvent::Opened {
                    protocol,
                    extensions,
                } => {
                    if socket.ready_state != CONNECTING {
                        continue;
                    }
                    socket.ready_state = OPEN;
                    socket.protocol = protocol;
                    socket.extensions = extensions;
                    let event =
                        self.new_event("Event", "open".to_string(), false, false, false, true);
                    self.dispatch(&event, events::ListenerTarget::Object(id))?;
                }
                SocketEvent::Text(_) | SocketEvent::Binary(_) if socket.ready_state != OPEN => {}
                SocketEvent::Text(text) => {
                    let origin = socket.origin.clone();
                    self.fire_socket_message(id, Value::String(text.into()), origin)?;
                }
                SocketEvent::Binary(bytes) => {
                    let data = if socket.binary_as_buffer {
                        Value::Bytes(Rc::new(RefCell::new(bytes.into())))
                    } else {
                        Value::Blob(Rc::new(BlobData {
                            bytes,
                            mime_type: String::new(),
                        }))
                    };
                    let origin = socket.origin.clone();
                    self.fire_socket_message(id, data, origin)?;
                }
                SocketEvent::Sent(length) => {
                    socket.buffered = socket.buffered.saturating_sub(length as u64);
                }
                SocketEvent::Closed {
                    code,
                    reason,
                    clean,
                } => {
                    if socket.ready_state == CLOSED {
                        continue;
                    }
                    socket.ready_state = CLOSED;
                    socket.connection = None;
                    if !clean {
                        let event =
                            self.new_event("Event", "error".to_string(), false, false, false, true);
                        self.dispatch(&event, events::ListenerTarget::Object(id))?;
                    }
                    let event = self.new_event(
                        "CloseEvent",
                        "close".to_string(),
                        false,
                        false,
                        false,
                        true,
                    );
                    event.set_field("wasClean", Value::Bool(clean));
                    event.set_field("code", Value::Number(f64::from(code)));
                    event.set_field("reason", Value::String(reason.into()));
                    let fired = self.dispatch(&event, events::ListenerTarget::Object(id));
                    self.held_targets.remove(&id);
                    fired?;
                }
            }
        }
        Ok(processed)
    }

    fn fire_socket_message(
        &mut self,
        id: u32,
        data: Value,
        origin: String,
    ) -> Result<(), ScriptError> {
        let event = self.new_event(
            "MessageEvent",
            "message".to_string(),
            false,
            false,
            false,
            true,
        );
        event.set_field("data", data);
        event.set_field("origin", Value::String(origin.into()));
        event.set_field("source", Value::Null);
        self.dispatch(&event, events::ListenerTarget::Object(id))?;
        Ok(())
    }
}
