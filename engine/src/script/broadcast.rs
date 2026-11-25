// BroadcastChannel: a message posted on one channel reaches the others of its name, in this
// page and, through the host, in the app's other windows

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::realm::{Builder, Native};
use super::*;
use crate::bridge::CommandReply;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChannelMember {
    Name,
    PostMessage,
    Close,
}

// the command that carries a message to the other windows, and the event it reaches them as
const RELAY: &str = "window:broadcast";
pub(super) const RELAYED: &str = "ceri:broadcast";

pub(super) struct ChannelInterfaces {
    prototype: ObjectHandle,
}

pub(super) fn create(
    build: &Builder,
    event_target: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> ChannelInterfaces {
    let prototype = new_object(JsObject::with_prototype(
        Some(event_target.clone()),
        ObjectClass::Ordinary,
    ));
    for (name, member, length) in [
        ("postMessage", ChannelMember::PostMessage, 1),
        ("close", ChannelMember::Close, 0),
    ] {
        let function = build.function(Native::Channel(member), name, length);
        prototype.borrow_mut().define(
            name.to_string(),
            Property {
                enumerable: true,
                ..Property::data(Value::Object(function))
            },
        );
    }
    let get = build.function(Native::Channel(ChannelMember::Name), "get name", 0);
    prototype.borrow_mut().define(
        "name".to_string(),
        Property {
            slot: Slot::accessor(Some(Value::Object(get)), None),
            enumerable: true,
            writable: false,
            configurable: true,
        },
    );
    prototype.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property::hidden(Value::String("BroadcastChannel".into())),
    );
    let constructor = build.constructor(
        Native::ChannelConstructor,
        "BroadcastChannel",
        1,
        &prototype,
    );
    globals.push(("BroadcastChannel", Value::Object(constructor)));
    ChannelInterfaces { prototype }
}

#[derive(Default)]
pub(super) struct Channels {
    // every channel made, by its event target: its name and whether it is still open
    made: HashMap<u32, (String, bool)>,
    // messages on their way: the channel name, the channel that posted, and the message
    inbox: Vec<(String, Option<u32>, CloneValue)>,
}

// what of a message the other windows can be sent, as JSON text: plain data, where a window
// of its own keeps maps, dates and bytes too
fn relayed(value: &CloneValue) -> Option<String> {
    Some(match value {
        CloneValue::Undefined | CloneValue::Null => "null".to_string(),
        CloneValue::Bool(value) => value.to_string(),
        CloneValue::Number(value) if value.is_finite() => value.to_string(),
        CloneValue::Number(_) => "null".to_string(),
        CloneValue::String(text) => json::quote(text),
        CloneValue::Array(items) => {
            let items: Option<Vec<_>> = items.iter().map(relayed).collect();
            format!("[{}]", items?.join(","))
        }
        CloneValue::Object(entries) => {
            let entries: Option<Vec<_>> = entries
                .iter()
                .filter(|(_, value)| *value != CloneValue::Undefined)
                .map(|(key, value)| Some(format!("{}:{}", json::quote(key), relayed(value)?)))
                .collect();
            format!("{{{}}}", entries?.join(","))
        }
        _ => return None,
    })
}

impl ScriptRuntime {
    pub(super) fn construct_channel(
        &mut self,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let Some(name) = arguments.first().cloned() else {
            return Err(self.type_error(
                "Failed to construct 'BroadcastChannel': 1 argument required, but only 0 present.",
            ));
        };
        let name = self.coerce_string(name)?.to_string();
        let prototype = match new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.channels.prototype.clone(),
        };
        let object = self.new_event_target(prototype);
        let ObjectClass::EventTarget(id) = object.borrow().class else {
            unreachable!("new_event_target makes event targets")
        };
        self.channels.made.insert(id, (name, true));
        Ok(Value::Object(object))
    }

    pub(super) fn call_channel(
        &mut self,
        member: ChannelMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let id = match &this {
            Value::Object(object) => match object.borrow().class {
                ObjectClass::EventTarget(id) if self.channels.made.contains_key(&id) => Some(id),
                _ => None,
            },
            _ => None,
        };
        let Some(id) = id else {
            return Err(self.type_error("Illegal invocation"));
        };
        let (name, open) = self.channels.made[&id].clone();
        Ok(match member {
            ChannelMember::Name => Value::String(name.into()),
            ChannelMember::Close => {
                if let Some(channel) = self.channels.made.get_mut(&id) {
                    channel.1 = false;
                }
                self.held_targets.remove(&id);
                Value::Undefined
            }
            ChannelMember::PostMessage => {
                if !open {
                    return Err(self.throw_value(dom_exception(
                        "InvalidStateError",
                        "Failed to execute 'postMessage' on 'BroadcastChannel': Channel is closed",
                    )));
                }
                let message = arguments.first().cloned().unwrap_or(Value::Undefined);
                let message = clone_payload(&message, MAX_CLONE_BYTES)?;
                if let Some(state) = &self.bridge
                    && state.permits(RELAY)
                    && let Some(data) = relayed(&message)
                {
                    let payload = format!("{{\"channel\":{},\"data\":{data}}}", json::quote(&name));
                    // nothing waits on the answer
                    let (sender, _) = std::sync::mpsc::channel();
                    state.send(RELAY, payload, CommandReply::new(0, sender));
                }
                self.channels.inbox.push((name, Some(id), message));
                self.schedule_native_task(Native::ChannelDeliver);
                Value::Undefined
            }
        })
    }

    // a message another window posted, as the host passes it on
    pub(super) fn receive_broadcast(&mut self, payload: &str) -> Result<usize, ScriptError> {
        let relayed = self.parse_json(payload)?;
        let name = self.get_property(&relayed, "channel")?.string_value();
        let data = self.get_property(&relayed, "data")?;
        let message = clone_payload(&data, MAX_CLONE_BYTES)?;
        self.channels.inbox.push((name, None, message));
        self.deliver_broadcasts()?;
        Ok(1)
    }

    // each waiting message fires at every open channel of its name but the one that posted it
    pub(super) fn deliver_broadcasts(&mut self) -> Result<Value, ScriptError> {
        for (name, sender, message) in std::mem::take(&mut self.channels.inbox) {
            // channels script let go of are forgotten here
            let live = &self.event_targets;
            self.channels
                .made
                .retain(|id, _| live.get(id).is_some_and(|target| target.is_alive()));
            let mut hearers: Vec<u32> = self
                .channels
                .made
                .iter()
                .filter(|(id, (other, open))| *open && *other == name && Some(**id) != sender)
                .map(|(id, _)| *id)
                .collect();
            hearers.sort_unstable();
            for id in hearers {
                let fields = events::HostEventFields {
                    value: Some(message.clone().into_value()?),
                    ..events::HostEventFields::default()
                };
                self.fire(events::ListenerTarget::Object(id), "message", fields)?;
            }
        }
        self.drain_microtasks()?;
        Ok(Value::Undefined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_reaches_the_other_channels_of_its_name() {
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
                const first = new BroadcastChannel("drag");
                const second = new BroadcastChannel("drag");
                const other = new BroadcastChannel("else");
                first.onmessage = () => seen.push("the first heard the other window");
                second.addEventListener("message", (event) => {
                    if (event.data.when) seen.push(event.constructor.name, event.data.x + ":" + event.data.when.getTime());
                    show();
                });
                other.onmessage = () => seen.push("another name heard it");
                seen.push(first.name, first instanceof EventTarget, String(first));
                first.postMessage({ x: 4, when: new Date(7) });
                seen.push("posted");
                window.late = new BroadcastChannel("drag");
                late.onmessage = (event) => (seen.push("late:" + event.data.x), show());
                window.shut = () => {
                    second.close();
                    try { second.postMessage(1); } catch (error) { seen.push(error.name); }
                    show();
                };
                show();
                "##,
            )
            .expect("script should run");
        runtime
            .advance_time(1)
            .expect("the message task should run");
        runtime
            .dispatch_host_event(RELAYED, r#"{"channel":"drag","data":{"x":9}}"#)
            .expect("a relayed message should arrive");
        runtime.execute("shut()").expect("script should run");
        let status = document.borrow().find_element_by_id("status").unwrap();
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some(
                "drag|true|[object BroadcastChannel]|posted|MessageEvent|4:7|late:4|the first heard the other window|late:9|InvalidStateError"
            )
        );
        assert_eq!(
            relayed(&CloneValue::Object(vec![
                (
                    "a".to_string(),
                    CloneValue::Array(vec![CloneValue::Number(1.0), CloneValue::Null])
                ),
                ("b".to_string(), CloneValue::String("q\"".to_string())),
            ]))
            .as_deref(),
            Some(r#"{"a":[1,null],"b":"q\""}"#)
        );
        assert_eq!(relayed(&CloneValue::Date(1.0)), None);
    }
}
