// EventSource: a stream of server-sent events over the host's off-thread fetch, parsed as the
// bytes arrive and fired as message events, reconnecting after the server's retry time

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::realm::{Builder, Native};
use super::*;
use crate::navigation::{ExternalEvent, ExternalReport, ExternalRequest};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceMember {
    Close,
    Url,
    ReadyState,
    WithCredentials,
}

const CONNECTING: u8 = 0;
const OPEN: u8 = 1;
const CLOSED: u8 = 2;

// the reconnection time before a server names one, in milliseconds
const DEFAULT_RETRY: u64 = 3000;

pub(super) struct SourceInterfaces {
    prototype: ObjectHandle,
}

pub(super) fn create(
    build: &Builder,
    event_target: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> SourceInterfaces {
    let prototype = new_object(JsObject::with_prototype(
        Some(event_target.clone()),
        ObjectClass::Ordinary,
    ));
    let close = build.function(Native::Source(SourceMember::Close), "close", 0);
    prototype.borrow_mut().define(
        "close".to_string(),
        Property {
            enumerable: true,
            ..Property::data(Value::Object(close))
        },
    );
    for (name, member) in [
        ("url", SourceMember::Url),
        ("readyState", SourceMember::ReadyState),
        ("withCredentials", SourceMember::WithCredentials),
    ] {
        let get = build.function(Native::Source(member), &format!("get {name}"), 0);
        prototype.borrow_mut().define(
            name.to_string(),
            Property {
                slot: Slot::accessor(Some(Value::Object(get)), None),
                enumerable: true,
                writable: false,
                configurable: true,
            },
        );
    }
    prototype.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property::hidden(Value::String("EventSource".into())),
    );
    let constructor = build.constructor(Native::SourceConstructor, "EventSource", 1, &prototype);
    for (name, value) in [("CONNECTING", 0.0), ("OPEN", 1.0), ("CLOSED", 2.0)] {
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
    globals.push(("EventSource", Value::Object(constructor)));
    SourceInterfaces { prototype }
}

pub(super) struct SourceState {
    url: String,
    origin: String,
    with_credentials: bool,
    ready_state: u8,
    retry: u64,
    last_event_id: String,
    // the connection in flight; a new one replaces it when the source reconnects
    events: Option<Receiver<ExternalEvent>>,
    // a line of the stream not yet ended, and the event its lines are building
    pending: Vec<u8>,
    data: String,
    event_type: String,
    event_id: Option<String>,
    // the first bytes of the stream, where a byte order mark is dropped
    started: bool,
}

impl ScriptRuntime {
    pub(super) fn construct_source(
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
        let Some(url) = crate::weburl::Url::parse(&reference, base.as_ref()) else {
            return Err(self.throw_value(dom_exception(
                "SyntaxError",
                &format!("Failed to construct 'EventSource': Cannot open an EventSource to '{reference}'. The URL is invalid."),
            )));
        };
        let with_credentials = match arguments.get(1) {
            Some(options) if !operations::is_primitive(options) => {
                self.get_property(options, "withCredentials")?.truthy()
            }
            _ => false,
        };
        let prototype = match new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.event_sources.prototype.clone(),
        };
        let object = self.new_event_target(prototype);
        let ObjectClass::EventTarget(id) = object.borrow().class else {
            unreachable!("new_event_target makes event targets")
        };
        // an open source stays alive with its listeners, as in browsers
        self.held_targets.insert(id, object.clone());
        self.event_sources.insert(
            id,
            SourceState {
                url: url.to_string(),
                origin: url.origin(),
                with_credentials,
                ready_state: CONNECTING,
                retry: DEFAULT_RETRY,
                last_event_id: String::new(),
                events: None,
                pending: Vec::new(),
                data: String::new(),
                event_type: String::new(),
                event_id: None,
                started: false,
            },
        );
        self.connect_source(id);
        Ok(Value::Object(object))
    }

    // starts the request; a source the host gives no off-thread request fails at the next turn
    fn connect_source(&mut self, id: u32) {
        let Some(source) = self.event_sources.get(&id) else {
            return;
        };
        let url = source.url.clone();
        let mut headers = BTreeMap::new();
        headers.insert("Accept".to_string(), "text/event-stream".to_string());
        headers.insert("Cache-Control".to_string(), "no-cache".to_string());
        if !source.last_event_id.is_empty() {
            headers.insert("Last-Event-ID".to_string(), source.last_event_id.clone());
        }
        let check = self.content_security_policy().url(
            crate::csp::Directive::Connect,
            &url,
            crate::csp::Initiator::default(),
        );
        let (sender, events) = channel();
        let waker = self.socket_waker();
        let started = self.permits(check)
            && match (
                self.resource_provider.as_ref(),
                ExternalRequest::new(url.clone(), "GET", headers, Vec::new()),
            ) {
                (Some(provider), Ok(request)) => {
                    let wake = waker.clone();
                    let report: ExternalReport = Box::new(move |event| {
                        let _ = sender.send(event);
                        if let Some(waker) = &wake {
                            waker();
                        }
                    });
                    provider.spawn_external(&request, report)
                }
                _ => false,
            };
        if !started {
            self.report(format!("EventSource connection to '{url}' failed"));
            let (failed, events) = channel();
            let _ = failed.send(ExternalEvent::Failed(
                "the host refused the connection".to_string(),
            ));
            if let Some(source) = self.event_sources.get_mut(&id) {
                source.events = Some(events);
            }
            if let Some(waker) = &waker {
                waker();
            }
            return;
        }
        if let Some(source) = self.event_sources.get_mut(&id) {
            source.events = Some(events);
            source.pending.clear();
            source.data.clear();
            source.event_type.clear();
            source.event_id = None;
            source.started = false;
        }
    }

    fn this_source(&mut self, this: &Value) -> Result<u32, ScriptError> {
        if let Value::Object(object) = this
            && let ObjectClass::EventTarget(id) = object.borrow().class
            && self.event_sources.contains_key(&id)
        {
            return Ok(id);
        }
        Err(self.type_error("Illegal invocation"))
    }

    pub(super) fn call_source(
        &mut self,
        member: SourceMember,
        this: Value,
    ) -> Result<Value, ScriptError> {
        let id = self.this_source(&this)?;
        let source = self
            .event_sources
            .get_mut(&id)
            .expect("checked by this_source");
        Ok(match member {
            SourceMember::Url => Value::String(source.url.clone().into()),
            SourceMember::ReadyState => Value::Number(f64::from(source.ready_state)),
            SourceMember::WithCredentials => Value::Bool(source.with_credentials),
            SourceMember::Close => {
                source.ready_state = CLOSED;
                source.events = None;
                self.held_targets.remove(&id);
                Value::Undefined
            }
        })
    }

    pub(super) fn reconnect_source(&mut self, id: u32) -> Result<Value, ScriptError> {
        if self
            .event_sources
            .get(&id)
            .is_some_and(|source| source.ready_state == CONNECTING && source.events.is_none())
        {
            self.connect_source(id);
        }
        Ok(Value::Undefined)
    }

    pub(super) fn pump_event_sources(&mut self) -> Result<usize, ScriptError> {
        let ids: Vec<u32> = self
            .event_sources
            .iter()
            .filter(|(_, source)| source.events.is_some())
            .map(|(id, _)| *id)
            .collect();
        let mut processed = 0;
        for id in ids {
            loop {
                let event = {
                    let Some(source) = self.event_sources.get(&id) else {
                        break;
                    };
                    let Some(events) = &source.events else {
                        break;
                    };
                    match events.try_recv() {
                        Ok(event) => event,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => ExternalEvent::Done,
                    }
                };
                processed += 1;
                match event {
                    ExternalEvent::Head {
                        status, mime_type, ..
                    } => {
                        if status != 200 || !mime_type.eq_ignore_ascii_case("text/event-stream") {
                            self.fail_source(id)?;
                            break;
                        }
                        if let Some(source) = self.event_sources.get_mut(&id) {
                            source.ready_state = OPEN;
                        }
                        let event =
                            self.new_event("Event", "open".to_string(), false, false, false, true);
                        self.dispatch(&event, events::ListenerTarget::Object(id))?;
                    }
                    ExternalEvent::Chunk(bytes) => self.feed_source(id, &bytes)?,
                    ExternalEvent::Done => {
                        self.lost_source(id)?;
                        break;
                    }
                    ExternalEvent::Failed(_) => {
                        let open = self
                            .event_sources
                            .get(&id)
                            .is_some_and(|source| source.ready_state == OPEN);
                        if open {
                            self.lost_source(id)?;
                        } else {
                            self.fail_source(id)?;
                        }
                        break;
                    }
                }
            }
        }
        Ok(processed)
    }

    // a connection that ends after it opened is tried again after the retry time
    fn lost_source(&mut self, id: u32) -> Result<(), ScriptError> {
        let retry = {
            let Some(source) = self.event_sources.get_mut(&id) else {
                return Ok(());
            };
            if source.ready_state == CLOSED {
                return Ok(());
            }
            source.ready_state = CONNECTING;
            source.events = None;
            source.retry
        };
        let event = self.new_event("Event", "error".to_string(), false, false, false, true);
        self.dispatch(&event, events::ListenerTarget::Object(id))?;
        let task = self.next_task_id;
        self.next_task_id = self.next_task_id.saturating_add(1);
        self.tasks.push(ScheduledTask {
            id: task,
            due: self.clock.saturating_add(retry),
            callback: Value::Builtin(Builtin::native(Native::SourceReconnect(id))),
            arguments: Vec::new(),
            kind: TaskKind::Timeout,
        });
        Ok(())
    }

    // a connection refused or answered with something other than an event stream ends the source
    fn fail_source(&mut self, id: u32) -> Result<(), ScriptError> {
        let Some(source) = self.event_sources.get_mut(&id) else {
            return Ok(());
        };
        if source.ready_state == CLOSED {
            return Ok(());
        }
        source.ready_state = CLOSED;
        source.events = None;
        let event = self.new_event("Event", "error".to_string(), false, false, false, true);
        let fired = self.dispatch(&event, events::ListenerTarget::Object(id));
        self.held_targets.remove(&id);
        fired.map(|_| ())
    }

    // splits what arrived into lines and acts on each whole one
    fn feed_source(&mut self, id: u32, bytes: &[u8]) -> Result<(), ScriptError> {
        let lines = {
            let Some(source) = self.event_sources.get_mut(&id) else {
                return Ok(());
            };
            source.pending.extend_from_slice(bytes);
            if !source.started && source.pending.len() >= 3 {
                if source.pending.starts_with(&[0xef, 0xbb, 0xbf]) {
                    source.pending.drain(..3);
                }
                source.started = true;
            }
            let mut lines = Vec::new();
            let mut start = 0;
            let mut index = 0;
            while index < source.pending.len() {
                match source.pending[index] {
                    b'\n' => {
                        lines.push(source.pending[start..index].to_vec());
                        start = index + 1;
                    }
                    b'\r' => {
                        // a carriage return at the end may be half of a pair still to come
                        if index + 1 == source.pending.len() {
                            break;
                        }
                        lines.push(source.pending[start..index].to_vec());
                        if source.pending[index + 1] == b'\n' {
                            index += 1;
                        }
                        start = index + 1;
                    }
                    _ => {}
                }
                index += 1;
            }
            source.pending.drain(..start);
            lines
        };
        for line in lines {
            self.source_line(id, &String::from_utf8_lossy(&line))?;
        }
        Ok(())
    }

    fn source_line(&mut self, id: u32, line: &str) -> Result<(), ScriptError> {
        if line.is_empty() {
            return self.dispatch_source_event(id);
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        let Some(source) = self.event_sources.get_mut(&id) else {
            return Ok(());
        };
        match field {
            "event" => source.event_type = value.to_string(),
            "data" => {
                source.data.push_str(value);
                source.data.push('\n');
            }
            "id" if !value.contains('\0') => source.event_id = Some(value.to_string()),
            "retry" if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                source.retry = value.parse().unwrap_or(source.retry);
            }
            _ => {}
        }
        Ok(())
    }

    fn dispatch_source_event(&mut self, id: u32) -> Result<(), ScriptError> {
        let (data, kind, last_event_id, origin) = {
            let Some(source) = self.event_sources.get_mut(&id) else {
                return Ok(());
            };
            if let Some(event_id) = source.event_id.take() {
                source.last_event_id = event_id;
            }
            let kind = std::mem::take(&mut source.event_type);
            let mut data = std::mem::take(&mut source.data);
            if data.is_empty() || source.ready_state == CLOSED {
                return Ok(());
            }
            data.pop();
            (
                data,
                if kind.is_empty() {
                    "message".to_string()
                } else {
                    kind
                },
                source.last_event_id.clone(),
                source.origin.clone(),
            )
        };
        let event = self.new_event("MessageEvent", kind, false, false, false, true);
        event.set_field("data", Value::String(data.into()));
        event.set_field("origin", Value::String(origin.into()));
        event.set_field("lastEventId", Value::String(last_event_id.into()));
        event.set_field("source", Value::Null);
        self.dispatch(&event, events::ListenerTarget::Object(id))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::{NavigationError, Resource, ResourceBundle, ResourceProvider};
    use std::time::{Duration, Instant};

    struct Events;

    impl ResourceProvider for Events {
        fn provide(&self, _url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
            Ok(None)
        }

        fn spawn_external(&self, request: &ExternalRequest, report: ExternalReport) -> bool {
            let resumed = request
                .headers()
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("last-event-id"))
                .map(|(_, value)| value.clone());
            std::thread::spawn(move || {
                report(ExternalEvent::Head {
                    status: 200,
                    mime_type: "text/event-stream".to_string(),
                    headers: BTreeMap::new(),
                });
                match resumed {
                    None => {
                        for chunk in [
                            ": hello\n\ndata: one\n\n",
                            "event: ping\r\ndata: a\r\ndata: b\r\nid: 7\r\n\r\nretry: 10\n",
                            "data: sp",
                            "lit\n\n",
                        ] {
                            report(ExternalEvent::Chunk(chunk.as_bytes().to_vec()));
                        }
                    }
                    Some(id) => report(ExternalEvent::Chunk(
                        format!("data: back {id}\n\n").into_bytes(),
                    )),
                }
                report(ExternalEvent::Done);
            });
            true
        }
    }

    #[test]
    fn event_sources_parse_the_stream_and_reconnect_with_the_last_id() {
        let document =
            crate::dom::Document::parse(r#"<p id="out">idle</p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources_and_storage_and_runtime_info(
            document.clone(),
            ResourceBundle::default(),
            Some(Rc::new(Events)),
            base,
            Rc::new(RefCell::new(StorageData::new())),
            Rc::new(RefCell::new(StorageData::new())),
            None,
        );
        runtime
            .execute(
                r#"
                const out = document.getElementById("out");
                const seen = [];
                const source = new EventSource("https://api.example.test/events");
                seen.push(source.readyState);
                source.onopen = () => seen.push("open");
                source.onmessage = (event) => {
                    seen.push(event.data + "@" + event.lastEventId);
                    if (event.data.startsWith("back")) {
                        source.close();
                        out.textContent = seen.join("|") + "|" + source.readyState;
                    }
                };
                source.addEventListener("ping", (event) => seen.push("ping:" + event.data.replace("\n", "+")));
                source.onerror = () => seen.push("error" + source.readyState);
                "#,
            )
            .expect("script should run");
        let text = |document: &Rc<RefCell<crate::dom::Document>>| {
            let node = document.borrow().find_element_by_id("out").unwrap();
            document
                .borrow()
                .element_text_content(node)
                .unwrap_or_default()
        };
        let started = Instant::now();
        while text(&document) == "idle" && started.elapsed() < Duration::from_secs(5) {
            runtime.pump().expect("the pump should run");
            runtime.advance_time(5).expect("timers should run");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            text(&document),
            "0|open|one@|ping:a+b|split@7|error0|open|back 7@7|2"
        );
    }
}
