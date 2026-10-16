// XMLHttpRequest over the same request path as fetch: the response is resolved when send()
// runs, and an asynchronous request reports it from a task, as the network would

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::operations::plain_object;
use super::realm::{Builder, Native};
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum XhrMember {
    Open,
    SetRequestHeader,
    Send,
    Abort,
    GetResponseHeader,
    GetAllResponseHeaders,
    OverrideMimeType,
    ReadyState,
    Status,
    StatusText,
    ResponseText,
    Response,
    ResponseUrl,
    ResponseType,
    SetResponseType,
    Timeout,
    SetTimeout,
    WithCredentials,
    SetWithCredentials,
    Upload,
}

const METHODS: &[(&str, XhrMember, u32)] = &[
    ("open", XhrMember::Open, 2),
    ("setRequestHeader", XhrMember::SetRequestHeader, 2),
    ("send", XhrMember::Send, 0),
    ("abort", XhrMember::Abort, 0),
    ("getResponseHeader", XhrMember::GetResponseHeader, 1),
    ("getAllResponseHeaders", XhrMember::GetAllResponseHeaders, 0),
    ("overrideMimeType", XhrMember::OverrideMimeType, 1),
];

const ACCESSORS: &[(&str, XhrMember, Option<XhrMember>)] = &[
    ("readyState", XhrMember::ReadyState, None),
    ("status", XhrMember::Status, None),
    ("statusText", XhrMember::StatusText, None),
    ("responseText", XhrMember::ResponseText, None),
    ("response", XhrMember::Response, None),
    ("responseURL", XhrMember::ResponseUrl, None),
    (
        "responseType",
        XhrMember::ResponseType,
        Some(XhrMember::SetResponseType),
    ),
    ("timeout", XhrMember::Timeout, Some(XhrMember::SetTimeout)),
    (
        "withCredentials",
        XhrMember::WithCredentials,
        Some(XhrMember::SetWithCredentials),
    ),
    ("upload", XhrMember::Upload, None),
];

const STATES: &[(&str, f64)] = &[
    ("UNSENT", 0.0),
    ("OPENED", 1.0),
    ("HEADERS_RECEIVED", 2.0),
    ("LOADING", 3.0),
    ("DONE", 4.0),
];

pub(super) struct XhrInterfaces {
    request: ObjectHandle,
    upload: ObjectHandle,
}

pub(super) fn create(
    build: &Builder,
    event_target: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> XhrInterfaces {
    let request = new_object(JsObject::with_prototype(
        Some(event_target.clone()),
        ObjectClass::Ordinary,
    ));
    for &(name, member, length) in METHODS {
        let function = build.function(Native::Xhr(member), name, length);
        request.borrow_mut().define(
            name.to_string(),
            Property {
                enumerable: true,
                ..Property::data(Value::Object(function))
            },
        );
    }
    for &(name, getter, setter) in ACCESSORS {
        let get = Value::Object(build.function(Native::Xhr(getter), &format!("get {name}"), 0));
        let set = setter.map(|setter| {
            Value::Object(build.function(Native::Xhr(setter), &format!("set {name}"), 1))
        });
        request.borrow_mut().define(
            name.to_string(),
            Property {
                slot: Slot::Accessor {
                    get: Some(get),
                    set,
                },
                enumerable: true,
                writable: false,
                configurable: true,
            },
        );
    }
    request.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property::hidden(Value::String("XMLHttpRequest".into())),
    );
    let constructor = build.constructor(Native::XhrConstructor, "XMLHttpRequest", 0, &request);
    for &(name, value) in STATES {
        for target in [&constructor, &request] {
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
    globals.push(("XMLHttpRequest", Value::Object(constructor)));
    let upload = new_object(JsObject::with_prototype(
        Some(event_target.clone()),
        ObjectClass::Ordinary,
    ));
    upload.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property::hidden(Value::String("XMLHttpRequestUpload".into())),
    );
    XhrInterfaces { request, upload }
}

// what a request answered with, captured when send() ran
struct XhrResponse {
    url: String,
    status: u16,
    status_text: String,
    mime_type: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

#[derive(Default)]
pub(super) struct XhrState {
    method: String,
    url: String,
    asynchronous: bool,
    headers: Vec<(String, String)>,
    ready_state: u8,
    sending: bool,
    response_type: String,
    mime_override: Option<String>,
    timeout: f64,
    with_credentials: bool,
    upload: Option<ObjectHandle>,
    // the answer waiting for its task, or what the page now sees
    pending: Option<Result<XhrResponse, ()>>,
    response: Option<XhrResponse>,
    failed: bool,
    // bumped by open() and abort(), so a stale delivery task does nothing
    generation: u64,
}

impl ScriptRuntime {
    pub(super) fn construct_xhr(&mut self, new_target: Value) -> Result<Value, ScriptError> {
        let prototype = match new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.xhr.request.clone(),
        };
        let object = self.new_event_target(prototype);
        let ObjectClass::EventTarget(id) = object.borrow().class else {
            unreachable!("new_event_target makes event targets")
        };
        self.xhr.insert(id, XhrState::default());
        Ok(Value::Object(object))
    }

    fn this_xhr(&mut self, this: &Value) -> Result<(u32, ObjectHandle), ScriptError> {
        if let Value::Object(object) = this
            && let ObjectClass::EventTarget(id) = object.borrow().class
            && self.xhr.contains_key(&id)
        {
            return Ok((id, object.clone()));
        }
        Err(self.type_error("Illegal invocation"))
    }

    fn xhr_failure(&mut self, name: &str, message: &str) -> ScriptError {
        self.throw_value(dom_exception(name, message))
    }

    // fires readystatechange or a progress event at the request
    fn xhr_event(
        &mut self,
        id: u32,
        event_type: &str,
        progress: Option<(u64, bool)>,
    ) -> Result<(), ScriptError> {
        let interface = if progress.is_some() {
            "ProgressEvent"
        } else {
            "Event"
        };
        let event = self.new_event(interface, event_type.to_string(), false, false, false, true);
        if let Some((loaded, computable)) = progress {
            event.set_field("loaded", Value::Number(loaded as f64));
            event.set_field(
                "total",
                Value::Number(if computable { loaded as f64 } else { 0.0 }),
            );
            event.set_field("lengthComputable", Value::Bool(computable));
        }
        self.dispatch(&event, events::ListenerTarget::Object(id))?;
        Ok(())
    }

    fn set_ready_state(&mut self, id: u32, state: u8) -> Result<(), ScriptError> {
        if let Some(xhr) = self.xhr.get_mut(&id) {
            xhr.ready_state = state;
        }
        self.xhr_event(id, "readystatechange", None)
    }

    // runs the request now; the answer waits in the state until it is reported
    fn perform_xhr(&mut self, id: u32, body: Value) -> Result<XhrResponse, ()> {
        let xhr = &self.xhr[&id];
        let headers = plain_object(
            xhr.headers
                .iter()
                .map(|(name, value)| (name.clone(), Value::String(value.clone().into()))),
        );
        let mut init = vec![
            (
                "method".to_string(),
                Value::String(xhr.method.clone().into()),
            ),
            ("headers".to_string(), headers),
        ];
        if !matches!(body, Value::Undefined | Value::Null)
            && !matches!(xhr.method.as_str(), "GET" | "HEAD")
        {
            init.push(("body".to_string(), body));
        }
        let url = Value::String(xhr.url.clone().into());
        let promise = self.fetch(&[url, plain_object(init)]);
        let outcome = match &promise.borrow().outcome {
            PromiseOutcome::Fulfilled(Value::Response(response)) => Ok(response.clone()),
            _ => Err(()),
        };
        let response = outcome?;
        let body = response.body.borrow().bytes.clone();
        let mut headers = response.headers.borrow().values.clone();
        // packaged resources carry their type beside the headers rather than in them
        if !response.mime_type.is_empty()
            && !headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("content-type"))
        {
            headers.insert("content-type".to_string(), response.mime_type.clone());
        }
        Ok(XhrResponse {
            url: response.url.clone(),
            status: response.status,
            status_text: response.status_text.clone(),
            mime_type: response.mime_type.clone(),
            headers,
            body,
        })
    }

    // reports a captured answer the way the network would: headers, body, then done
    fn finish_xhr(
        &mut self,
        id: u32,
        answer: Result<XhrResponse, ()>,
        asynchronous: bool,
    ) -> Result<(), ScriptError> {
        let Some(xhr) = self.xhr.get_mut(&id) else {
            return Ok(());
        };
        xhr.sending = false;
        match answer {
            Ok(response) => {
                let length = response.body.len() as u64;
                xhr.response = Some(response);
                if asynchronous {
                    self.set_ready_state(id, 2)?;
                    self.set_ready_state(id, 3)?;
                    self.xhr_event(id, "progress", Some((length, true)))?;
                }
                self.set_ready_state(id, 4)?;
                self.xhr_event(id, "load", Some((length, true)))?;
                self.xhr_event(id, "loadend", Some((length, true)))?;
            }
            Err(()) => {
                xhr.failed = true;
                xhr.response = None;
                self.set_ready_state(id, 4)?;
                self.xhr_event(id, "error", Some((0, false)))?;
                self.xhr_event(id, "loadend", Some((0, false)))?;
            }
        }
        Ok(())
    }

    // the task an asynchronous send() queued
    pub(super) fn deliver_xhr(&mut self, id: u32, generation: u64) -> Result<Value, ScriptError> {
        let answer = match self.xhr.get_mut(&id) {
            Some(xhr) if xhr.generation == generation => xhr.pending.take(),
            _ => None,
        };
        if let Some(answer) = answer {
            self.finish_xhr(id, answer, true)?;
        }
        if !self
            .event_listeners
            .contains_key(&events::ListenerTarget::Object(id))
        {
            self.held_targets.remove(&id);
        }
        Ok(Value::Undefined)
    }

    fn response_text(&self, xhr: &XhrState) -> String {
        xhr.response
            .as_ref()
            .map(|response| String::from_utf8_lossy(&response.body).into_owned())
            .unwrap_or_default()
    }

    pub(super) fn call_xhr(
        &mut self,
        member: XhrMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let (id, object) = self.this_xhr(&this)?;
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        let state = |runtime: &Self| runtime.xhr[&id].ready_state;
        Ok(match member {
            XhrMember::Open => {
                let method = self.coerce_string(first)?;
                let url =
                    self.coerce_string(arguments.get(1).cloned().unwrap_or(Value::Undefined))?;
                let asynchronous = arguments.get(2).is_none_or(Value::truthy);
                let upper = method.to_ascii_uppercase();
                if matches!(upper.as_str(), "CONNECT" | "TRACE" | "TRACK") {
                    return Err(self.xhr_failure(
                        "SecurityError",
                        &format!("'{method}' HTTP method is unsupported."),
                    ));
                }
                let normalized = match upper.as_str() {
                    "DELETE" | "GET" | "HEAD" | "OPTIONS" | "POST" | "PUT" | "PATCH" => upper,
                    _ => method,
                };
                let xhr = self.xhr.get_mut(&id).expect("checked by this_xhr");
                xhr.method = normalized;
                xhr.url = url;
                xhr.asynchronous = asynchronous;
                xhr.headers.clear();
                xhr.sending = false;
                xhr.response = None;
                xhr.pending = None;
                xhr.failed = false;
                xhr.generation += 1;
                if xhr.ready_state != 1 {
                    self.set_ready_state(id, 1)?;
                }
                Value::Undefined
            }
            XhrMember::SetRequestHeader => {
                let name = self.coerce_string(first)?;
                let value =
                    self.coerce_string(arguments.get(1).cloned().unwrap_or(Value::Undefined))?;
                let xhr = &self.xhr[&id];
                if xhr.ready_state != 1 || xhr.sending {
                    return Err(
                        self.xhr_failure("InvalidStateError", "The object's state must be OPENED.")
                    );
                }
                let xhr = self.xhr.get_mut(&id).expect("checked by this_xhr");
                let value = value.trim().to_string();
                match xhr
                    .headers
                    .iter_mut()
                    .find(|(existing, _)| existing.eq_ignore_ascii_case(&name))
                {
                    Some((_, existing)) => {
                        existing.push_str(", ");
                        existing.push_str(&value);
                    }
                    None => xhr.headers.push((name, value)),
                }
                Value::Undefined
            }
            XhrMember::Send => {
                let xhr = &self.xhr[&id];
                if xhr.ready_state != 1 || xhr.sending {
                    return Err(
                        self.xhr_failure("InvalidStateError", "The object's state must be OPENED.")
                    );
                }
                let asynchronous = xhr.asynchronous;
                let answer = self.perform_xhr(id, first);
                if asynchronous {
                    let xhr = self.xhr.get_mut(&id).expect("checked by this_xhr");
                    xhr.sending = true;
                    xhr.pending = Some(answer);
                    let generation = xhr.generation;
                    // a request in flight keeps its object alive until it reports
                    self.held_targets.insert(id, object);
                    self.xhr_event(id, "loadstart", Some((0, false)))?;
                    self.schedule_native_task(Native::XhrDeliver(id, generation));
                } else {
                    self.finish_xhr(id, answer, false)?;
                    if self.xhr[&id].failed {
                        return Err(self.xhr_failure(
                            "NetworkError",
                            "Failed to execute 'send' on 'XMLHttpRequest'.",
                        ));
                    }
                }
                Value::Undefined
            }
            XhrMember::Abort => {
                let current = state(self);
                let sending = self.xhr[&id].sending;
                let xhr = self.xhr.get_mut(&id).expect("checked by this_xhr");
                xhr.generation += 1;
                xhr.pending = None;
                xhr.response = None;
                xhr.sending = false;
                if (current == 1 && sending) || current == 2 || current == 3 {
                    self.set_ready_state(id, 4)?;
                    self.xhr_event(id, "abort", Some((0, false)))?;
                    self.xhr_event(id, "loadend", Some((0, false)))?;
                }
                if state(self) == 4 {
                    self.xhr
                        .get_mut(&id)
                        .expect("checked by this_xhr")
                        .ready_state = 0;
                }
                Value::Undefined
            }
            XhrMember::GetResponseHeader => {
                let name = self.coerce_string(first)?.to_ascii_lowercase();
                self.xhr[&id]
                    .response
                    .as_ref()
                    .and_then(|response| {
                        response
                            .headers
                            .iter()
                            .find(|(key, _)| key.eq_ignore_ascii_case(&name))
                    })
                    .map_or(Value::Null, |(_, value)| {
                        Value::String(value.clone().into())
                    })
            }
            XhrMember::GetAllResponseHeaders => {
                let text = self.xhr[&id]
                    .response
                    .as_ref()
                    .map(|response| {
                        response
                            .headers
                            .iter()
                            .map(|(name, value)| {
                                format!("{}: {value}\r\n", name.to_ascii_lowercase())
                            })
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                Value::String(text.into())
            }
            XhrMember::OverrideMimeType => {
                let mime = self.coerce_string(first)?;
                self.xhr
                    .get_mut(&id)
                    .expect("checked by this_xhr")
                    .mime_override = Some(mime);
                Value::Undefined
            }
            XhrMember::ReadyState => Value::Number(f64::from(state(self))),
            XhrMember::Status => Value::Number(
                self.xhr[&id]
                    .response
                    .as_ref()
                    .filter(|_| state(self) >= 2)
                    .map_or(0.0, |response| f64::from(response.status)),
            ),
            XhrMember::StatusText => Value::String(
                self.xhr[&id]
                    .response
                    .as_ref()
                    .map(|response| response.status_text.clone())
                    .unwrap_or_default()
                    .into(),
            ),
            XhrMember::ResponseUrl => Value::String(
                self.xhr[&id]
                    .response
                    .as_ref()
                    .map(|response| response.url.clone())
                    .unwrap_or_default()
                    .into(),
            ),
            XhrMember::ResponseText => {
                let xhr = &self.xhr[&id];
                if !matches!(xhr.response_type.as_str(), "" | "text") {
                    return Err(self.xhr_failure(
                        "InvalidStateError",
                        "The value is only accessible if the object's 'responseType' is '' or 'text'.",
                    ));
                }
                Value::String(self.response_text(xhr).into())
            }
            XhrMember::Response => {
                let xhr = &self.xhr[&id];
                if state(self) != 4 && !matches!(xhr.response_type.as_str(), "" | "text") {
                    return Ok(Value::Null);
                }
                match xhr.response_type.as_str() {
                    "" | "text" => Value::String(self.response_text(xhr).into()),
                    "json" => {
                        let text = self.response_text(xhr);
                        if xhr.response.is_none() {
                            Value::Null
                        } else {
                            self.parse_json(&text).unwrap_or(Value::Null)
                        }
                    }
                    "arraybuffer" => match &xhr.response {
                        Some(response) => {
                            Value::Bytes(Rc::new(RefCell::new(response.body.clone())))
                        }
                        None => Value::Null,
                    },
                    "blob" => match &xhr.response {
                        Some(response) => Value::Blob(Rc::new(BlobData {
                            bytes: response.body.clone(),
                            mime_type: xhr
                                .mime_override
                                .clone()
                                .unwrap_or_else(|| response.mime_type.clone()),
                        })),
                        None => Value::Null,
                    },
                    _ => Value::Null,
                }
            }
            XhrMember::ResponseType => Value::String(self.xhr[&id].response_type.clone().into()),
            XhrMember::SetResponseType => {
                let kind = self.coerce_string(first)?;
                if matches!(state(self), 3 | 4) {
                    return Err(self.xhr_failure(
                        "InvalidStateError",
                        "The response type cannot be set if the object's state is LOADING or DONE.",
                    ));
                }
                if matches!(
                    kind.as_str(),
                    "" | "text" | "json" | "arraybuffer" | "blob" | "document"
                ) {
                    self.xhr
                        .get_mut(&id)
                        .expect("checked by this_xhr")
                        .response_type = kind;
                }
                Value::Undefined
            }
            XhrMember::Timeout => Value::Number(self.xhr[&id].timeout),
            XhrMember::SetTimeout => {
                let timeout = self.coerce_number(first)?;
                self.xhr.get_mut(&id).expect("checked by this_xhr").timeout = if timeout.is_finite()
                {
                    timeout.max(0.0).trunc()
                } else {
                    0.0
                };
                Value::Undefined
            }
            XhrMember::WithCredentials => Value::Bool(self.xhr[&id].with_credentials),
            XhrMember::SetWithCredentials => {
                self.xhr
                    .get_mut(&id)
                    .expect("checked by this_xhr")
                    .with_credentials = first.truthy();
                Value::Undefined
            }
            XhrMember::Upload => {
                if let Some(upload) = &self.xhr[&id].upload {
                    return Ok(Value::Object(upload.clone()));
                }
                let upload = self.new_event_target(self.intrinsics.xhr.upload.clone());
                self.xhr.get_mut(&id).expect("checked by this_xhr").upload = Some(upload.clone());
                Value::Object(upload)
            }
        })
    }
}
