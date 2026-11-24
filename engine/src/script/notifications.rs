// the Notification interface: a notice goes to the host as notification:show when the app's
// capabilities grant it to the page, and the host's answer fires show or error on it

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::realm::{Builder, Native};
use super::*;
use crate::bridge::{CommandError, CommandReply, CommandResult};
use std::sync::mpsc::{Receiver, Sender, channel};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeMember {
    Close,
    Title,
    Dir,
    Lang,
    Body,
    Tag,
    Image,
    Icon,
    Badge,
    Timestamp,
    Renotify,
    Silent,
    RequireInteraction,
    Data,
    Actions,
    Permission,
    MaxActions,
    RequestPermission,
}

const ACCESSORS: &[(&str, NoticeMember)] = &[
    ("title", NoticeMember::Title),
    ("dir", NoticeMember::Dir),
    ("lang", NoticeMember::Lang),
    ("body", NoticeMember::Body),
    ("tag", NoticeMember::Tag),
    ("image", NoticeMember::Image),
    ("icon", NoticeMember::Icon),
    ("badge", NoticeMember::Badge),
    ("timestamp", NoticeMember::Timestamp),
    ("renotify", NoticeMember::Renotify),
    ("silent", NoticeMember::Silent),
    ("requireInteraction", NoticeMember::RequireInteraction),
    ("data", NoticeMember::Data),
    ("actions", NoticeMember::Actions),
];

const SHOW: &str = "notification:show";

pub(super) struct NoticeInterfaces {
    prototype: ObjectHandle,
}

fn accessor(build: &Builder, target: &ObjectHandle, name: &str, member: NoticeMember) {
    let get = build.function(Native::Notice(member), &format!("get {name}"), 0);
    target.borrow_mut().define(
        name.to_string(),
        Property {
            slot: Slot::accessor(Some(Value::Object(get)), None),
            enumerable: true,
            writable: false,
            configurable: true,
        },
    );
}

fn method(build: &Builder, target: &ObjectHandle, name: &str, member: NoticeMember, length: u32) {
    let function = build.function(Native::Notice(member), name, length);
    target.borrow_mut().define(
        name.to_string(),
        Property {
            enumerable: true,
            ..Property::data(Value::Object(function))
        },
    );
}

pub(super) fn create(
    build: &Builder,
    event_target: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> NoticeInterfaces {
    let prototype = new_object(JsObject::with_prototype(
        Some(event_target.clone()),
        ObjectClass::Ordinary,
    ));
    method(build, &prototype, "close", NoticeMember::Close, 0);
    for &(name, member) in ACCESSORS {
        accessor(build, &prototype, name, member);
    }
    prototype.borrow_mut().define(
        "@@toStringTag".to_string(),
        Property::hidden(Value::String("Notification".into())),
    );
    let constructor = build.constructor(Native::NoticeConstructor, "Notification", 1, &prototype);
    accessor(build, &constructor, "permission", NoticeMember::Permission);
    accessor(build, &constructor, "maxActions", NoticeMember::MaxActions);
    method(
        build,
        &constructor,
        "requestPermission",
        NoticeMember::RequestPermission,
        0,
    );
    globals.push(("Notification", Value::Object(constructor)));
    NoticeInterfaces { prototype }
}

struct NoticeState {
    title: String,
    dir: &'static str,
    lang: String,
    body: String,
    tag: String,
    image: String,
    icon: String,
    badge: String,
    timestamp: f64,
    renotify: bool,
    silent: Option<bool>,
    require_interaction: bool,
    data: CloneValue,
    shown: bool,
}

pub(super) struct Notices {
    states: HashMap<u32, NoticeState>,
    replies: Sender<CommandResult>,
    answers: Receiver<CommandResult>,
    // close events waiting for the next pump, as the spec queues them
    closing: Vec<u32>,
}

impl Default for Notices {
    fn default() -> Self {
        let (replies, answers) = channel();
        Self {
            states: HashMap::default(),
            replies,
            answers,
            closing: Vec::new(),
        }
    }
}

impl Notices {
    pub(super) fn retain(&mut self, live: impl Fn(&u32) -> bool) {
        self.states.retain(|id, _| live(id));
    }
}

impl ScriptRuntime {
    // pages get no prompt: the app's capabilities decide, as tauri's do on desktop
    pub(super) fn notice_permission(&self) -> &'static str {
        match &self.bridge {
            Some(state) if state.permits(SHOW) => "granted",
            _ => "denied",
        }
    }

    fn notice_url(&mut self, options: &Value, key: &str) -> Result<String, ScriptError> {
        let value = self.notice_option(options, key)?;
        if matches!(value, Value::Undefined) {
            return Ok(String::new());
        }
        let text = self.coerce_string(value)?;
        let base = self
            .base_url
            .as_ref()
            .and_then(|base| crate::weburl::Url::parse(&base.to_string(), None));
        Ok(crate::weburl::Url::parse(&text, base.as_ref())
            .map(|url| url.to_string())
            .unwrap_or_default())
    }

    fn notice_option(&mut self, options: &Value, key: &str) -> Result<Value, ScriptError> {
        match options {
            Value::Undefined | Value::Null => Ok(Value::Undefined),
            options => self.get_property(options, key),
        }
    }

    fn notice_text(&mut self, options: &Value, key: &str) -> Result<String, ScriptError> {
        match self.notice_option(options, key)? {
            Value::Undefined => Ok(String::new()),
            value => self.coerce_string(value),
        }
    }

    pub(super) fn construct_notice(
        &mut self,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let Some(title) = arguments.first().cloned() else {
            return Err(self.type_error(
                "Failed to construct 'Notification': 1 argument required, but only 0 present.",
            ));
        };
        let title = self.coerce_string(title)?;
        let options = arguments.get(1).cloned().unwrap_or(Value::Undefined);
        if !matches!(options, Value::Undefined | Value::Null)
            && super::operations::is_primitive(&options)
        {
            return Err(self.type_error(
                "Failed to construct 'Notification': The provided value is not of type 'NotificationOptions'.",
            ));
        }
        // dictionary members are read in name order, as webidl converts them
        let actions = match self.notice_option(&options, "actions")? {
            Value::Undefined => Vec::new(),
            value => self.iterate_to_vec(value)?,
        };
        let badge = self.notice_url(&options, "badge")?;
        let body = self.notice_text(&options, "body")?;
        let data = match self.notice_option(&options, "data")? {
            Value::Undefined => Value::Null,
            value => value,
        };
        let dir = match self.notice_option(&options, "dir")? {
            Value::Undefined => "auto",
            value => match self.coerce_string(value)?.as_str() {
                "auto" => "auto",
                "ltr" => "ltr",
                "rtl" => "rtl",
                other => {
                    return Err(self.type_error(format!(
                        "Failed to construct 'Notification': Failed to read the 'dir' property from 'NotificationOptions': The provided value '{other}' is not a valid enum value of type NotificationDirection."
                    )));
                }
            },
        };
        let icon = self.notice_url(&options, "icon")?;
        let image = self.notice_url(&options, "image")?;
        let lang = self.notice_text(&options, "lang")?;
        let renotify = self.notice_option(&options, "renotify")?.truthy();
        let require_interaction = self.notice_option(&options, "requireInteraction")?.truthy();
        let silent = match self.notice_option(&options, "silent")? {
            Value::Undefined | Value::Null => None,
            value => Some(value.truthy()),
        };
        let tag = self.notice_text(&options, "tag")?;
        let timestamp = match self.notice_option(&options, "timestamp")? {
            Value::Undefined => self.now_milliseconds().floor(),
            value => self.coerce_number(value)?.trunc(),
        };
        if !actions.is_empty() {
            return Err(self.type_error(
                "Failed to construct 'Notification': Actions are only supported for persistent notifications shown using ServiceWorkerRegistration.showNotification().",
            ));
        }
        if renotify && tag.is_empty() {
            return Err(self.type_error(
                "Failed to construct 'Notification': Notifications which set the renotify flag must specify a non-empty tag.",
            ));
        }
        let data = clone_payload(&data, MAX_CLONE_BYTES)?;
        let prototype = match new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.notices.prototype.clone(),
        };
        let object = self.new_event_target(prototype);
        let ObjectClass::EventTarget(id) = object.borrow().class else {
            unreachable!("new_event_target makes event targets")
        };
        // a notice waiting on the host stays alive for its show or error listeners
        self.held_targets.insert(id, object.clone());
        let payload = format!(
            "{{\"title\":{},\"body\":{}}}",
            super::json::quote(&title),
            super::json::quote(&body)
        );
        self.notices.states.insert(
            id,
            NoticeState {
                title,
                dir,
                lang,
                body,
                tag,
                image,
                icon,
                badge,
                timestamp,
                renotify,
                silent,
                require_interaction,
                data,
                shown: false,
            },
        );
        let reply = CommandReply::new(u64::from(id), self.notices.replies.clone());
        match &self.bridge {
            Some(state) if state.permits(SHOW) => state.send(SHOW, payload, reply),
            _ => reply.send(Err(CommandError::new(
                "denied",
                "notifications are not granted",
            ))),
        }
        Ok(Value::Object(object))
    }

    pub(super) fn call_notice(
        &mut self,
        member: NoticeMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        match member {
            NoticeMember::Permission => {
                return Ok(Value::String(self.notice_permission().into()));
            }
            NoticeMember::MaxActions => return Ok(Value::Number(0.0)),
            NoticeMember::RequestPermission => {
                let permission = Value::String(self.notice_permission().into());
                if let Some(callback) = arguments.first().filter(|value| is_callable(value)) {
                    self.call_function(
                        callback.clone(),
                        Value::Undefined,
                        vec![permission.clone()],
                    )?;
                }
                return Ok(Value::Promise(self.fulfilled_promise(permission)));
            }
            _ => {}
        }
        let id = match &this {
            Value::Object(object) => match object.borrow().class {
                ObjectClass::EventTarget(id) if self.notices.states.contains_key(&id) => Some(id),
                _ => None,
            },
            _ => None,
        };
        let Some(id) = id else {
            return Err(self.type_error("Illegal invocation"));
        };
        let notice = &self.notices.states[&id];
        Ok(match member {
            NoticeMember::Close => {
                self.close_notice(id);
                Value::Undefined
            }
            NoticeMember::Title => Value::String(notice.title.clone().into()),
            NoticeMember::Dir => Value::String(notice.dir.into()),
            NoticeMember::Lang => Value::String(notice.lang.clone().into()),
            NoticeMember::Body => Value::String(notice.body.clone().into()),
            NoticeMember::Tag => Value::String(notice.tag.clone().into()),
            NoticeMember::Image => Value::String(notice.image.clone().into()),
            NoticeMember::Icon => Value::String(notice.icon.clone().into()),
            NoticeMember::Badge => Value::String(notice.badge.clone().into()),
            NoticeMember::Timestamp => Value::Number(notice.timestamp),
            NoticeMember::Renotify => Value::Bool(notice.renotify),
            NoticeMember::Silent => notice.silent.map_or(Value::Null, Value::Bool),
            NoticeMember::RequireInteraction => Value::Bool(notice.require_interaction),
            NoticeMember::Data => notice.data.clone().into_value()?,
            NoticeMember::Actions => Self::new_array(Vec::new()),
            NoticeMember::Permission
            | NoticeMember::MaxActions
            | NoticeMember::RequestPermission => unreachable!("answered above"),
        })
    }

    // a shown notice leaves the list and hears close on the next pump; any other is ignored
    fn close_notice(&mut self, id: u32) {
        if let Some(notice) = self.notices.states.get_mut(&id)
            && notice.shown
        {
            notice.shown = false;
            self.notices.closing.push(id);
        }
    }

    pub(super) fn pump_notices(&mut self) -> Result<usize, ScriptError> {
        let mut fired = 0;
        while let Ok((id, answer)) = self.notices.answers.try_recv() {
            let id = id as u32;
            let Some(notice) = self.notices.states.get(&id) else {
                continue;
            };
            let shown = answer.is_ok();
            // a shown notice stays alive until it closes, for its close listeners
            let _held = (!shown).then(|| self.held_targets.remove(&id));
            if shown {
                // a newer notice under the same tag takes the older one's place
                let tag = notice.tag.clone();
                let replaced: Vec<u32> = self
                    .notices
                    .states
                    .iter()
                    .filter(|(other, state)| {
                        **other != id && state.shown && !tag.is_empty() && state.tag == tag
                    })
                    .map(|(other, _)| *other)
                    .collect();
                for other in replaced {
                    self.close_notice(other);
                }
                if let Some(notice) = self.notices.states.get_mut(&id) {
                    notice.shown = true;
                }
            }
            let name = if shown { "show" } else { "error" };
            let event = self.new_event("Event", name.to_string(), false, false, false, true);
            self.dispatch(&event, events::ListenerTarget::Object(id))?;
            fired += 1;
        }
        for id in std::mem::take(&mut self.notices.closing) {
            let _held = self.held_targets.remove(&id);
            let event = self.new_event("Event", "close".to_string(), false, false, false, true);
            self.dispatch(&event, events::ListenerTarget::Object(id))?;
            fired += 1;
        }
        Ok(fired)
    }
}
