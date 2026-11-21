// MessageChannel and its two entangled ports, and window.postMessage to the page itself;
// a message is cloned when posted and arrives from a task as a MessageEvent

use super::object::{JsObject, ObjectClass, ObjectHandle, Property};
use super::realm::{Builder, Native};
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PortMember {
    PostMessage,
    Start,
    Close,
}

pub(super) struct PortInterfaces {
    prototype: ObjectHandle,
}

pub(super) fn create(
    build: &Builder,
    event_target: &ObjectHandle,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> PortInterfaces {
    let prototype = new_object(JsObject::with_prototype(
        Some(event_target.clone()),
        ObjectClass::Ordinary,
    ));
    for (name, member, length) in [
        ("postMessage", PortMember::PostMessage, 1),
        ("start", PortMember::Start, 0),
        ("close", PortMember::Close, 0),
    ] {
        let function = build.function(Native::Port(member), name, length);
        prototype.borrow_mut().define(
            name.to_string(),
            Property {
                enumerable: true,
                ..Property::data(Value::Object(function))
            },
        );
    }
    prototype.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property::hidden(Value::String("MessagePort".into())),
    );
    let port = build.constructor(Native::PortConstructor, "MessagePort", 0, &prototype);
    globals.push(("MessagePort", Value::Object(port)));
    let channel_prototype = new_object(JsObject::with_prototype(
        Some(object_prototype.clone()),
        ObjectClass::Ordinary,
    ));
    channel_prototype.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property::hidden(Value::String("MessageChannel".into())),
    );
    let channel = build.constructor(
        Native::MessageChannelConstructor,
        "MessageChannel",
        0,
        &channel_prototype,
    );
    globals.push(("MessageChannel", Value::Object(channel)));
    PortInterfaces { prototype }
}

#[derive(Default)]
pub(super) struct Ports {
    // each port's partner, while the pair is open
    partners: HashMap<u32, Option<u32>>,
    // messages on their way: the port that hears it, or none for the window, and the message
    inbox: Vec<(Option<u32>, CloneValue)>,
}

impl ScriptRuntime {
    // a fresh channel holds two ports, each posting to the other
    pub(super) fn construct_message_channel(
        &mut self,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let channel_prototype = match new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => return Err(self.type_error("MessageChannel needs new")),
        };
        let mut ports = Vec::new();
        for _ in 0..2 {
            let object = self.new_event_target(self.intrinsics.ports.prototype.clone());
            let ObjectClass::EventTarget(id) = object.borrow().class else {
                unreachable!("new_event_target makes event targets")
            };
            ports.push((id, object));
        }
        let (first, second) = (ports[0].0, ports[1].0);
        self.ports.partners.insert(first, Some(second));
        self.ports.partners.insert(second, Some(first));
        let channel = new_object(JsObject::with_prototype(
            Some(channel_prototype),
            ObjectClass::Ordinary,
        ));
        for (name, (_, port)) in ["port1", "port2"].into_iter().zip(ports) {
            channel.borrow_mut().define(
                name.to_string(),
                Property {
                    enumerable: true,
                    writable: false,
                    ..Property::data(Value::Object(port))
                },
            );
        }
        Ok(Value::Object(channel))
    }

    pub(super) fn call_port(
        &mut self,
        member: PortMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let id = match &this {
            Value::Object(object) => match object.borrow().class {
                ObjectClass::EventTarget(id) if self.ports.partners.contains_key(&id) => Some(id),
                _ => None,
            },
            _ => None,
        };
        let Some(id) = id else {
            return Err(self.type_error("Illegal invocation"));
        };
        match member {
            PortMember::Start => {}
            // closing either end leaves both unable to send, and lets go of the pair
            PortMember::Close => {
                if let Some(Some(other)) = self.ports.partners.insert(id, None) {
                    self.ports.partners.insert(other, None);
                    self.held_targets.remove(&other);
                }
                self.held_targets.remove(&id);
            }
            PortMember::PostMessage => {
                let message = arguments.first().cloned().unwrap_or(Value::Undefined);
                let message = clone_payload(&message, MAX_CLONE_BYTES)?;
                if let Some(Some(other)) = self.ports.partners.get(&id).copied() {
                    // a port with a message on its way stays alive to hear it
                    if let Some(object) = self.event_targets.get(&other).and_then(Weak::upgrade) {
                        self.held_targets.insert(other, object);
                    }
                    self.ports.inbox.push((Some(other), message));
                    self.schedule_native_task(Native::PortDeliver);
                }
            }
        }
        Ok(Value::Undefined)
    }

    // window.postMessage, which a page can only aim at itself
    pub(super) fn post_window_message(
        &mut self,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let message = arguments.first().cloned().unwrap_or(Value::Undefined);
        let target = match arguments.get(1).map(Value::resolve) {
            Some(Value::String(origin)) => Some(origin.to_string()),
            Some(Value::Object(options)) => match options.borrow().get("targetOrigin") {
                Some(Value::String(origin)) => Some(origin.to_string()),
                _ => None,
            },
            _ => None,
        };
        let own = crate::navigation::APP_ORIGIN;
        match target.as_deref().map(str::trim) {
            None | Some("*" | "/") => {}
            Some(origin) if origin.trim_end_matches('/') == own => {}
            Some(origin) if crate::weburl::Url::parse(origin, None).is_none() => {
                return Err(self.throw_value(dom_exception(
                    "SyntaxError",
                    &format!(
                        "Failed to execute 'postMessage' on 'Window': Invalid target origin '{origin}' in a call to 'postMessage'."
                    ),
                )));
            }
            // a message for another origin is dropped, as a browser drops it
            Some(_) => return Ok(Value::Undefined),
        }
        let message = clone_payload(&message, MAX_CLONE_BYTES)?;
        self.ports.inbox.push((None, message));
        self.schedule_native_task(Native::PortDeliver);
        Ok(Value::Undefined)
    }

    pub(super) fn deliver_port_messages(&mut self) -> Result<Value, ScriptError> {
        for (port, message) in std::mem::take(&mut self.ports.inbox) {
            let (target, origin, source) = match port {
                Some(id) => (
                    events::ListenerTarget::Object(id),
                    String::new(),
                    Value::Null,
                ),
                None => (
                    events::ListenerTarget::Window,
                    crate::navigation::APP_ORIGIN.to_string(),
                    Value::Window,
                ),
            };
            let fields = events::HostEventFields {
                value: Some(message.into_value()?),
                origin: Some(origin),
                source: Some(source),
                ..events::HostEventFields::default()
            };
            self.fire(target, "message", fields)?;
        }
        self.drain_microtasks()?;
        Ok(Value::Undefined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_post_to_each_other_and_a_page_to_itself() {
        let document =
            crate::dom::Document::parse(r#"<html><body><p id="status"></p></body></html>"#)
                .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const seen = [];
                const show = () => (document.getElementById("status").textContent = seen.join("|"));
                const { port1, port2 } = new MessageChannel();
                port2.onmessage = (event) => {
                    seen.push("port2:" + event.data.n, event.constructor.name, event.origin === "");
                    port2.postMessage("back");
                };
                port1.addEventListener("message", (event) => {
                    seen.push("port1:" + event.data);
                    port1.close();
                    port2.postMessage("after close");
                    show();
                });
                port1.start();
                const sent = { n: 1 };
                port1.postMessage(sent);
                sent.n = 2;
                addEventListener("message", (event) => {
                    seen.push("window:" + event.data, event.origin, event.source === window);
                    show();
                });
                postMessage("hi", "*");
                postMessage("elsewhere", "https://other.example");
                window.postMessage("own", "https://app.ceri.invalid");
                try { postMessage("x", "not a url"); } catch (error) { seen.push(error.name); }
                seen.push(String(new MessageChannel()), port1 instanceof EventTarget);
                show();
                "##,
            )
            .expect("script should run");
        for _ in 0..4 {
            runtime.advance_time(1).expect("message tasks should run");
        }
        let status = document.borrow().find_element_by_id("status").unwrap();
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some(
                "SyntaxError|[object MessageChannel]|true|port2:1|MessageEvent|true|window:hi|https://app.ceri.invalid|true|window:own|https://app.ceri.invalid|true|port1:back"
            )
        );
    }
}
