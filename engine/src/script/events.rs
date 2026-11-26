// DOM events: event objects with interface prototypes, one listener registry for nodes and
// the window, and the capture, target and bubble dispatch of the DOM standard

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::operations::{is_callable, is_primitive};
use super::realm::{Builder, Native};
use super::*;
use std::cell::Cell;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EventAccess {
    Get,
    Set,
    Call,
}

#[derive(Clone, Copy, Debug)]
enum Field {
    // type, target and the rest of the dispatch state
    Core,
    // a core member that scripts may also write
    CoreWritable,
    Number(f64),
    Flag,
    Text,
    // any value, null until set
    Any,
    Method(u32),
}

// one member of one event interface; the entry points into the static tables
#[derive(Clone, Copy, Debug)]
pub(super) struct EventMember {
    entry: &'static (&'static str, Field),
    access: EventAccess,
}

struct EventInterface {
    name: &'static str,
    parent: &'static str,
    members: &'static [(&'static str, Field)],
}

use Field::{Any, Core, CoreWritable, Flag, Method, Number, Text};

const MODIFIERS: [(&str, Field); 5] = [
    ("ctrlKey", Flag),
    ("shiftKey", Flag),
    ("altKey", Flag),
    ("metaKey", Flag),
    ("getModifierState", Method(1)),
];

static EVENT_INTERFACES: &[EventInterface] = &[
    EventInterface {
        name: "Event",
        parent: "",
        members: &[
            ("type", Core),
            ("target", Core),
            ("currentTarget", Core),
            ("srcElement", Core),
            ("eventPhase", Core),
            ("bubbles", Core),
            ("cancelable", Core),
            ("defaultPrevented", Core),
            ("composed", Core),
            ("isTrusted", Core),
            ("timeStamp", Core),
            ("returnValue", CoreWritable),
            ("cancelBubble", CoreWritable),
            ("preventDefault", Method(0)),
            ("stopPropagation", Method(0)),
            ("stopImmediatePropagation", Method(0)),
            ("composedPath", Method(0)),
            ("initEvent", Method(3)),
        ],
    },
    EventInterface {
        name: "CustomEvent",
        parent: "Event",
        members: &[("detail", Any), ("initCustomEvent", Method(4))],
    },
    EventInterface {
        name: "UIEvent",
        parent: "Event",
        members: &[
            ("view", Any),
            ("detail", Number(0.0)),
            ("which", Number(0.0)),
        ],
    },
    EventInterface {
        name: "FocusEvent",
        parent: "UIEvent",
        members: &[("relatedTarget", Any)],
    },
    EventInterface {
        name: "MouseEvent",
        parent: "UIEvent",
        members: &[
            ("screenX", Number(0.0)),
            ("screenY", Number(0.0)),
            ("clientX", Number(0.0)),
            ("clientY", Number(0.0)),
            ("pageX", Number(0.0)),
            ("pageY", Number(0.0)),
            ("offsetX", Number(0.0)),
            ("offsetY", Number(0.0)),
            ("x", Number(0.0)),
            ("y", Number(0.0)),
            ("movementX", Number(0.0)),
            ("movementY", Number(0.0)),
            ("button", Number(0.0)),
            ("buttons", Number(0.0)),
            ("relatedTarget", Any),
            MODIFIERS[0],
            MODIFIERS[1],
            MODIFIERS[2],
            MODIFIERS[3],
            MODIFIERS[4],
        ],
    },
    EventInterface {
        name: "PointerEvent",
        parent: "MouseEvent",
        members: &[
            ("pointerId", Number(0.0)),
            ("width", Number(1.0)),
            ("height", Number(1.0)),
            ("pressure", Number(0.0)),
            ("tangentialPressure", Number(0.0)),
            ("tiltX", Number(0.0)),
            ("tiltY", Number(0.0)),
            ("twist", Number(0.0)),
            ("pointerType", Text),
            ("isPrimary", Flag),
        ],
    },
    EventInterface {
        name: "DragEvent",
        parent: "MouseEvent",
        members: &[("dataTransfer", Any)],
    },
    EventInterface {
        name: "WheelEvent",
        parent: "MouseEvent",
        members: &[
            ("deltaX", Number(0.0)),
            ("deltaY", Number(0.0)),
            ("deltaZ", Number(0.0)),
            ("deltaMode", Number(0.0)),
        ],
    },
    EventInterface {
        name: "KeyboardEvent",
        parent: "UIEvent",
        members: &[
            ("key", Text),
            ("code", Text),
            ("location", Number(0.0)),
            ("repeat", Flag),
            ("isComposing", Flag),
            ("charCode", Number(0.0)),
            ("keyCode", Number(0.0)),
            MODIFIERS[0],
            MODIFIERS[1],
            MODIFIERS[2],
            MODIFIERS[3],
            MODIFIERS[4],
        ],
    },
    EventInterface {
        name: "InputEvent",
        parent: "UIEvent",
        members: &[("data", Any), ("inputType", Text), ("isComposing", Flag)],
    },
    EventInterface {
        name: "CompositionEvent",
        parent: "UIEvent",
        members: &[("data", Text)],
    },
    EventInterface {
        name: "TransitionEvent",
        parent: "Event",
        members: &[
            ("propertyName", Text),
            ("elapsedTime", Number(0.0)),
            ("pseudoElement", Text),
        ],
    },
    EventInterface {
        name: "AnimationEvent",
        parent: "Event",
        members: &[
            ("animationName", Text),
            ("elapsedTime", Number(0.0)),
            ("pseudoElement", Text),
        ],
    },
    EventInterface {
        name: "MessageEvent",
        parent: "Event",
        members: &[
            ("data", Any),
            ("origin", Text),
            ("lastEventId", Text),
            ("source", Any),
        ],
    },
    EventInterface {
        name: "CloseEvent",
        parent: "Event",
        members: &[("wasClean", Flag), ("code", Number(0.0)), ("reason", Text)],
    },
    EventInterface {
        name: "ErrorEvent",
        parent: "Event",
        members: &[
            ("message", Text),
            ("filename", Text),
            ("lineno", Number(0.0)),
            ("colno", Number(0.0)),
            ("error", Any),
        ],
    },
    EventInterface {
        name: "PromiseRejectionEvent",
        parent: "Event",
        members: &[("promise", Any), ("reason", Any)],
    },
    EventInterface {
        name: "ClipboardEvent",
        parent: "Event",
        members: &[("clipboardData", Any)],
    },
    EventInterface {
        name: "ToggleEvent",
        parent: "Event",
        members: &[("oldState", Text), ("newState", Text)],
    },
    EventInterface {
        name: "PopStateEvent",
        parent: "Event",
        members: &[("state", Any)],
    },
    EventInterface {
        name: "HashChangeEvent",
        parent: "Event",
        members: &[("oldURL", Text), ("newURL", Text)],
    },
    EventInterface {
        name: "ProgressEvent",
        parent: "Event",
        members: &[
            ("lengthComputable", Flag),
            ("loaded", Number(0.0)),
            ("total", Number(0.0)),
        ],
    },
    EventInterface {
        name: "MediaQueryListEvent",
        parent: "Event",
        members: &[("media", Text), ("matches", Flag)],
    },
    EventInterface {
        name: "SubmitEvent",
        parent: "Event",
        members: &[("submitter", Any)],
    },
    EventInterface {
        name: "StorageEvent",
        parent: "Event",
        members: &[
            ("key", Any),
            ("oldValue", Any),
            ("newValue", Any),
            ("url", Text),
            ("storageArea", Any),
        ],
    },
    EventInterface {
        name: "FormDataEvent",
        parent: "Event",
        members: &[("formData", Any)],
    },
];

const PHASES: &[(&str, f64)] = &[
    ("NONE", 0.0),
    ("CAPTURING_PHASE", 1.0),
    ("AT_TARGET", 2.0),
    ("BUBBLING_PHASE", 3.0),
];

pub(super) struct EventInterfaces {
    prototypes: HashMap<&'static str, ObjectHandle>,
}

impl EventInterfaces {
    fn prototype(&self, name: &str) -> ObjectHandle {
        self.prototypes
            .get(name)
            .cloned()
            .unwrap_or_else(|| self.prototypes["Event"].clone())
    }
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> EventInterfaces {
    let mut prototypes: HashMap<&'static str, ObjectHandle> = HashMap::default();
    for interface in EVENT_INTERFACES {
        let parent = if interface.parent.is_empty() {
            object_prototype.clone()
        } else {
            prototypes[interface.parent].clone()
        };
        let prototype = new_object(JsObject::with_prototype(
            Some(parent),
            ObjectClass::Ordinary,
        ));
        for entry in interface.members {
            let (name, field) = *entry;
            let member = |access| Native::Event(EventMember { entry, access });
            if let Method(length) = field {
                let function = build.function(member(EventAccess::Call), name, length);
                prototype.borrow_mut().define(
                    name.to_string(),
                    Property {
                        enumerable: true,
                        ..Property::data(Value::Object(function))
                    },
                );
                continue;
            }
            let get =
                Value::Object(build.function(member(EventAccess::Get), &format!("get {name}"), 0));
            let set = matches!(field, CoreWritable).then(|| {
                Value::Object(build.function(member(EventAccess::Set), &format!("set {name}"), 1))
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
            Property {
                writable: false,
                ..Property::hidden(Value::String(interface.name.into()))
            },
        );
        let constructor = build.constructor(
            Native::EventConstructor(interface.name),
            interface.name,
            1,
            &prototype,
        );
        if interface.name == "Event" {
            for &(name, value) in PHASES {
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
        }
        globals.push((interface.name, Value::Object(constructor)));
        prototypes.insert(interface.name, prototype);
    }
    EventInterfaces { prototypes }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CallbackKind {
    Listener,
    Handler,
    // window.onerror, which takes the message, source, line, column and error
    ErrorHandler,
}

// where listeners live: a node, or the window
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum ListenerTarget {
    Node(NodeId),
    Window,
    // a script object that is an EventTarget, such as `new EventTarget()` or a MediaQueryList
    Object(u32),
}

pub(super) struct Listener {
    pub(super) event: String,
    // the callback, or nothing for the slot an on* handler property holds
    callback: Option<Value>,
    capture: bool,
    once: bool,
    passive: bool,
    pub(super) signal: Option<AbortSignalHandle>,
    removed: Rc<Cell<bool>>,
}

impl Listener {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        if let Some(cb) = &self.callback {
            tracer.trace_value(cb);
        }
        if let Some(signal) = &self.signal {
            tracer.trace_abort_signal(signal);
        }
    }
}

#[derive(Default)]
struct DispatchState {
    target: Option<Value>,
    current_target: Option<Value>,
    phase: u8,
    stop: bool,
    stop_immediate: bool,
    canceled: bool,
    passive: bool,
    dispatching: bool,
    path: Vec<Value>,
    // the node dispatch started at, kept for retargeting across shadow boundaries
    origin: Option<NodeId>,
}

struct EventMeta {
    event_type: String,
    bubbles: bool,
    cancelable: bool,
    initialized: bool,
}

pub(super) struct EventRecord {
    // the wrapper that carries the interface prototype and expando properties
    pub(super) object: ObjectHandle,
    interface: &'static str,
    meta: RefCell<EventMeta>,
    composed: bool,
    trusted: bool,
    time_stamp: f64,
    // interface fields such as key, clientX or detail, from the host or an init dictionary
    fields: RefCell<HashMap<String, Value>>,
    state: RefCell<DispatchState>,
}

impl EventRecord {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        tracer.mark_object(self.object.clone());
        for value in self.fields.borrow().values() {
            tracer.trace_value(value);
        }
        let state = self.state.borrow();
        if let Some(target) = &state.target {
            tracer.trace_value(target);
        }
        if let Some(current) = &state.current_target {
            tracer.trace_value(current);
        }
        for value in &state.path {
            tracer.trace_value(value);
        }
    }

    pub(super) fn set_field(&self, name: &str, value: Value) {
        self.fields.borrow_mut().insert(name.to_string(), value);
    }

    pub(super) fn event_type(&self) -> String {
        self.meta.borrow().event_type.clone()
    }

    pub(super) fn canceled(&self) -> bool {
        self.state.borrow().canceled
    }

    pub(super) fn field(&self, name: &str) -> Option<Value> {
        self.fields.borrow().get(name).cloned()
    }
}

// the interface, bubbles and cancelable of an event the host fires
fn event_defaults(event_type: &str) -> (&'static str, bool, bool) {
    match event_type {
        "click" | "auxclick" | "contextmenu" => ("PointerEvent", true, true),
        "dblclick" | "mousedown" | "mouseup" | "mousemove" | "mouseover" | "mouseout" => {
            ("MouseEvent", true, true)
        }
        "mouseenter" | "mouseleave" => ("MouseEvent", false, false),
        "pointerdown" | "pointerup" | "pointermove" | "pointerover" | "pointerout" => {
            ("PointerEvent", true, true)
        }
        "pointercancel" | "gotpointercapture" | "lostpointercapture" => {
            ("PointerEvent", true, false)
        }
        "pointerenter" | "pointerleave" => ("PointerEvent", false, false),
        "wheel" => ("WheelEvent", true, true),
        "dragstart" => ("DragEvent", true, true),
        "keydown" | "keyup" | "keypress" => ("KeyboardEvent", true, true),
        "beforeinput" => ("InputEvent", true, true),
        "input" => ("InputEvent", true, false),
        "focus" | "blur" => ("FocusEvent", false, false),
        "focusin" | "focusout" => ("FocusEvent", true, false),
        "compositionstart" => ("CompositionEvent", true, true),
        "compositionupdate" | "compositionend" => ("CompositionEvent", true, false),
        "transitionrun" | "transitionstart" | "transitionend" | "transitioncancel" => {
            ("TransitionEvent", true, false)
        }
        "animationstart" | "animationend" | "animationiteration" | "animationcancel" => {
            ("AnimationEvent", true, false)
        }
        "submit" => ("SubmitEvent", true, true),
        "reset" => ("Event", true, true),
        "change" | "select" | "DOMContentLoaded" => ("Event", true, false),
        "invalid" => ("Event", false, true),
        "copy" | "cut" | "paste" => ("ClipboardEvent", true, true),
        "popstate" => ("PopStateEvent", false, false),
        "beforetoggle" => ("ToggleEvent", false, true),
        "toggle" => ("ToggleEvent", false, false),
        "hashchange" => ("HashChangeEvent", false, false),
        "message" | "messageerror" => ("MessageEvent", false, false),
        "error" => ("ErrorEvent", false, true),
        "unhandledrejection" | "rejectionhandled" => ("PromiseRejectionEvent", false, true),
        "resize" => ("UIEvent", false, false),
        _ => ("Event", false, false),
    }
}

// a KeyboardEvent code for a key, assuming a US layout
fn key_code_name(key: &str) -> String {
    let mut characters = key.chars();
    match (characters.next(), characters.next()) {
        (Some(letter), None) if letter.is_ascii_alphabetic() => {
            format!("Key{}", letter.to_ascii_uppercase())
        }
        (Some(digit), None) if digit.is_ascii_digit() => format!("Digit{digit}"),
        (Some(' '), None) => "Space".to_string(),
        (Some(symbol), None) => match symbol {
            '-' | '_' => "Minus",
            '=' | '+' => "Equal",
            '[' | '{' => "BracketLeft",
            ']' | '}' => "BracketRight",
            ';' | ':' => "Semicolon",
            '\'' | '"' => "Quote",
            ',' | '<' => "Comma",
            '.' | '>' => "Period",
            '/' | '?' => "Slash",
            '\\' | '|' => "Backslash",
            '`' | '~' => "Backquote",
            _ => "",
        }
        .to_string(),
        _ => match key {
            "Shift" => "ShiftLeft",
            "Control" => "ControlLeft",
            "Alt" => "AltLeft",
            "Meta" => "MetaLeft",
            other => other,
        }
        .to_string(),
    }
}

// where the pointer is and which buttons are down, as the host last reported
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct PointerState {
    x: f32,
    y: f32,
    // the button of the current press: 0 primary, 1 auxiliary, 2 secondary
    button: i16,
    buttons: u16,
    // a canceled pointerdown suppresses compatibility mouse events until the pointerup
    suppress_mouse: bool,
    // the element pointer events go to instead of the one under the pointer
    capture: Option<NodeId>,
    // what script asked for, which takes over at the next pointer event
    pending_capture: Option<NodeId>,
    // the capture the last release had, which its click goes to
    released_capture: Option<NodeId>,
    // keyboard modifiers active for pointer, mouse, wheel, and drag events
    modifiers: KeyModifiers,
    // wheel delta carried while dispatching a trusted wheel event
    wheel_delta: (f32, f32),
}

// the legacy keyCode of a key, which older code still compares against
fn legacy_key_code(key: &str) -> f64 {
    let mut characters = key.chars();
    if let (Some(character), None) = (characters.next(), characters.next()) {
        return match character {
            letter if letter.is_ascii_alphabetic() => f64::from(letter.to_ascii_uppercase() as u8),
            digit if digit.is_ascii_digit() => f64::from(digit as u8),
            ' ' => 32.0,
            _ => 0.0,
        };
    }
    match key {
        "Backspace" => 8.0,
        "Tab" => 9.0,
        "Enter" => 13.0,
        "Shift" => 16.0,
        "Control" => 17.0,
        "Alt" => 18.0,
        "Escape" => 27.0,
        "PageUp" => 33.0,
        "PageDown" => 34.0,
        "End" => 35.0,
        "Home" => 36.0,
        "ArrowLeft" => 37.0,
        "ArrowUp" => 38.0,
        "ArrowRight" => 39.0,
        "ArrowDown" => 40.0,
        "Delete" => 46.0,
        _ => 0.0,
    }
}

// fields a host sets on a trusted event
#[derive(Default)]
pub(super) struct HostEventFields {
    pub(super) key: String,
    pub(super) data: String,
    pub(super) modifiers: KeyModifiers,
    pub(super) value: Option<Value>,
    pub(super) state: Option<Value>,
    pub(super) message: Option<String>,
    pub(super) error: Option<Value>,
    pub(super) related_target: Option<Value>,
    // a MessageEvent's origin and source
    pub(super) origin: Option<String>,
    pub(super) source: Option<Value>,
}

impl ScriptRuntime {
    pub(super) fn new_event(
        &mut self,
        interface: &'static str,
        event_type: String,
        bubbles: bool,
        cancelable: bool,
        composed: bool,
        trusted: bool,
    ) -> Rc<EventRecord> {
        Rc::new(EventRecord {
            object: new_object(JsObject::with_prototype(
                Some(self.intrinsics.events.prototype(interface)),
                ObjectClass::Ordinary,
            )),
            interface,
            meta: RefCell::new(EventMeta {
                event_type,
                bubbles,
                cancelable,
                initialized: true,
            }),
            composed,
            trusted,
            time_stamp: self.clock as f64,
            fields: RefCell::new(HashMap::default()),
            state: RefCell::new(DispatchState::default()),
        })
    }

    // a trusted event for something the host saw happen, with its usual interface and flags
    pub(super) fn host_event(
        &mut self,
        event_type: &str,
        fields: HostEventFields,
    ) -> Rc<EventRecord> {
        let (interface, bubbles, cancelable) = event_defaults(event_type);
        let event = self.new_event(
            interface,
            event_type.to_string(),
            bubbles,
            cancelable,
            true,
            true,
        );
        {
            let mut values = event.fields.borrow_mut();
            let mut set = |name: &str, value: Value| {
                values.insert(name.to_string(), value);
            };
            match interface {
                "KeyboardEvent" => {
                    let (code, repeat) = self
                        .key_detail
                        .clone()
                        .unwrap_or_else(|| (key_code_name(&fields.key), false));
                    let key_code = legacy_key_code(&fields.key);
                    set("code", Value::String(code.into()));
                    set("key", Value::String(fields.key.clone().into()));
                    set("repeat", Value::Bool(repeat));
                    set("keyCode", Value::Number(key_code));
                    set("which", Value::Number(key_code));
                }
                "InputEvent" => {
                    set(
                        "data",
                        if fields.data.is_empty() {
                            Value::Null
                        } else {
                            Value::String(fields.data.clone().into())
                        },
                    );
                    let input_type = if fields.data.is_empty() {
                        ""
                    } else {
                        "insertText"
                    };
                    set("inputType", Value::String(input_type.into()));
                }
                "CompositionEvent" => set("data", Value::String(fields.data.clone().into())),
                "PointerEvent" | "MouseEvent" | "WheelEvent" | "DragEvent" => {
                    let pointer = self.pointer;
                    let scroll = self.document_scroll_offset();
                    for name in ["clientX", "x", "screenX", "offsetX"] {
                        set(name, Value::Number(f64::from(pointer.x)));
                    }
                    for name in ["clientY", "y", "screenY", "offsetY"] {
                        set(name, Value::Number(f64::from(pointer.y)));
                    }
                    set("pageX", Value::Number(f64::from(pointer.x + scroll.0)));
                    set("pageY", Value::Number(f64::from(pointer.y + scroll.1)));
                    let button = match event_type {
                        "pointerdown" | "pointerup" | "mousedown" | "mouseup" | "auxclick"
                        | "contextmenu" => pointer.button,
                        // a pointer event with no button change reports -1
                        other if other.starts_with("pointer") => -1,
                        _ => 0,
                    };
                    set("button", Value::Number(f64::from(button)));
                    set("which", Value::Number(f64::from(button + 1)));
                    set("buttons", Value::Number(f64::from(pointer.buttons)));
                    let detail = click_detail_for(event_type, self.click_detail);
                    set("detail", Value::Number(f64::from(detail)));
                    if interface == "PointerEvent" {
                        set("pointerId", Value::Number(1.0));
                        set("pointerType", Value::String("mouse".into()));
                        set("isPrimary", Value::Bool(true));
                    }
                    if interface == "WheelEvent" {
                        set("deltaX", Value::Number(f64::from(pointer.wheel_delta.0)));
                        set("deltaY", Value::Number(f64::from(pointer.wheel_delta.1)));
                        set("deltaZ", Value::Number(0.0));
                        set("deltaMode", Value::Number(0.0));
                    }
                }
                "TransitionEvent" | "AnimationEvent" => {
                    if let Some(motion) = &self.motion_detail {
                        let name = if interface == "TransitionEvent" {
                            "propertyName"
                        } else {
                            "animationName"
                        };
                        set(name, Value::String(motion.0.clone().into()));
                        set("elapsedTime", Value::Number(motion.1));
                    }
                }
                _ => {}
            }
            if matches!(
                interface,
                "KeyboardEvent" | "InputEvent" | "CompositionEvent"
            ) {
                set("isComposing", Value::Bool(self.composition.is_some()));
            }
            let modifiers = if matches!(
                interface,
                "PointerEvent" | "MouseEvent" | "WheelEvent" | "DragEvent"
            ) {
                self.pointer.modifiers
            } else {
                fields.modifiers
            };
            for (name, on) in [
                ("ctrlKey", modifiers.control),
                ("shiftKey", modifiers.shift),
                ("altKey", modifiers.alt),
                ("metaKey", modifiers.meta),
            ] {
                set(name, Value::Bool(on));
            }
            // a rejection event carries its promise and reason where others carry data and error
            let rejection = interface == "PromiseRejectionEvent";
            if let Some(value) = fields.value {
                let name = match interface {
                    "PromiseRejectionEvent" => "promise",
                    "ClipboardEvent" => "clipboardData",
                    _ => "data",
                };
                set(name, value);
            }
            if let Some(state) = fields.state {
                set("state", state);
            }
            if let Some(message) = fields.message {
                set("message", Value::String(message.into()));
            }
            if let Some(error) = fields.error {
                set(if rejection { "reason" } else { "error" }, error);
            }
            if let Some(related) = fields.related_target {
                set("relatedTarget", related);
            }
            if let Some(origin) = fields.origin {
                set("origin", Value::String(origin.into()));
            }
            if let Some(source) = fields.source {
                set("source", source);
            }
            set("view", Value::Window);
        }
        event
    }

    pub(super) fn listener_target(&self, value: &Value) -> Option<ListenerTarget> {
        match value.resolve() {
            Value::Element(handle) => Some(ListenerTarget::Node(handle.id)),
            Value::Document => Some(ListenerTarget::Node(self.document_node())),
            Value::Window => Some(ListenerTarget::Window),
            Value::Object(object) => match object.borrow().class {
                ObjectClass::EventTarget(id) => Some(ListenerTarget::Object(id)),
                _ => None,
            },
            _ => None,
        }
    }

    fn target_value(&self, target: ListenerTarget) -> Value {
        match target {
            ListenerTarget::Node(node) => self.node_value(node),
            ListenerTarget::Window => Value::Window,
            ListenerTarget::Object(id) => self
                .event_targets
                .get(&id)
                .and_then(super::object::ObjectWeak::upgrade)
                .map_or(Value::Null, Value::Object),
        }
    }

    // a fresh object that can hold listeners, with the given prototype
    pub(super) fn new_event_target(&mut self, prototype: ObjectHandle) -> ObjectHandle {
        self.next_event_target += 1;
        let id = self.next_event_target;
        let object = new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::EventTarget(id),
        ));
        self.event_targets.insert(id, object.downgrade());
        object
    }

    // forgets listeners of event target objects that script can no longer reach
    pub(super) fn prune_event_targets(&mut self) {
        let before = self.event_targets.len();
        self.event_targets.retain(|_, object| object.is_alive());
        if self.event_targets.len() == before {
            return;
        }
        let live = &self.event_targets;
        let alive = |target: &ListenerTarget| match target {
            ListenerTarget::Object(id) => live.contains_key(id),
            _ => true,
        };
        self.event_listeners.retain(|target, _| alive(target));
        self.event_handlers.retain(|(target, _), _| alive(target));
        self.xhr.retain(|id, _| live.contains_key(id));
        self.sockets.states.retain(|id, _| live.contains_key(id));
        self.notices.retain(|id| live.contains_key(id));
        self.prune_audio();
    }

    // the targets an event visits, innermost first; the window follows a connected node
    // the targets an event visits, innermost first; a composed event leaves a shadow tree
    // through its host, and the window follows a connected node
    fn event_path(
        &self,
        target: ListenerTarget,
        event_type: &str,
        composed: bool,
    ) -> Vec<ListenerTarget> {
        let node = match target {
            ListenerTarget::Node(node) => node,
            other => return vec![other],
        };
        let document = self.document.borrow();
        let mut path = vec![ListenerTarget::Node(node)];
        let mut current = node;
        loop {
            let next = if composed {
                document.event_parent(current)
            } else if document.has_shadow_roots() && document.assigned_slot(current).is_some() {
                document.assigned_slot(current)
            } else {
                document.parent(current)
            };
            let Some(next) = next else {
                break;
            };
            path.push(ListenerTarget::Node(next));
            current = next;
        }
        if current == document.root() && event_type != "load" {
            path.push(ListenerTarget::Window);
        }
        path
    }

    // the target as listeners at a node see it: a node inside a shadow tree they are not
    // part of shows up as its host
    fn retarget(&self, origin: NodeId, listener: ListenerTarget) -> NodeId {
        let document = self.document.borrow();
        let mut target = origin;
        loop {
            let root = document.tree_root(target);
            let Some((host, _)) = document.shadow_host(root) else {
                return target;
            };
            let inside = match listener {
                ListenerTarget::Node(node) => {
                    let mut current = Some(node);
                    let mut found = false;
                    while let Some(candidate) = current {
                        if candidate == root {
                            found = true;
                            break;
                        }
                        current = document.event_parent(candidate);
                    }
                    found
                }
                _ => false,
            };
            if inside {
                return target;
            }
            target = host;
        }
    }

    pub(super) fn add_event_listener(
        &mut self,
        target: ListenerTarget,
        arguments: &[Value],
    ) -> Result<(), ScriptError> {
        let event = match arguments.first() {
            Some(value) => self.coerce_string(value.clone())?,
            None => {
                return Err(self.type_error(
                    "Failed to execute 'addEventListener' on 'EventTarget': 2 arguments required, but only 0 present.",
                ));
            }
        };
        let callback = arguments.get(1).map(Value::resolve).unwrap_or(Value::Null);
        if matches!(callback, Value::Null | Value::Undefined) {
            return Ok(());
        }
        if is_primitive(&callback) {
            return Err(self.type_error(
                "Failed to execute 'addEventListener' on 'EventTarget': parameter 2 is not of type 'Object'.",
            ));
        }
        let (capture, once, passive, signal) = self.listener_flags(arguments.get(2))?;
        if signal
            .as_ref()
            .is_some_and(|signal| signal.borrow().aborted)
        {
            return Ok(());
        }
        let listeners = self.event_listeners.entry(target).or_default();
        let duplicate = listeners.iter().any(|listener| {
            listener.event == event
                && listener.capture == capture
                && listener
                    .callback
                    .as_ref()
                    .is_some_and(|existing| values_equal(existing, &callback))
        });
        if let ListenerTarget::Object(id) = target
            && let Some(object) = self
                .event_targets
                .get(&id)
                .and_then(super::object::ObjectWeak::upgrade)
        {
            self.held_targets.insert(id, object);
        }
        let listeners = self.event_listeners.entry(target).or_default();
        if !duplicate {
            listeners.push(Listener {
                event,
                callback: Some(callback),
                capture,
                once,
                passive,
                signal,
                removed: Rc::new(Cell::new(false)),
            });
        }
        Ok(())
    }

    // capture, once, passive and signal from a boolean or an options object
    fn listener_flags(
        &mut self,
        options: Option<&Value>,
    ) -> Result<(bool, bool, bool, Option<AbortSignalHandle>), ScriptError> {
        let options = options.map(Value::resolve).unwrap_or(Value::Undefined);
        if is_primitive(&options) {
            return Ok((options.truthy(), false, false, None));
        }
        let flag = |runtime: &mut Self, name: &str| -> Result<bool, ScriptError> {
            Ok(runtime.get_property(&options, name)?.truthy())
        };
        let capture = flag(self, "capture")?;
        let once = flag(self, "once")?;
        let passive = flag(self, "passive")?;
        let signal = match self.get_property(&options, "signal")? {
            Value::Undefined => None,
            Value::AbortSignal(signal) => Some(signal),
            _ => {
                return Err(self.type_error(
                    "Failed to execute 'addEventListener' on 'EventTarget': member signal is not of type 'AbortSignal'.",
                ));
            }
        };
        Ok((capture, once, passive, signal))
    }

    pub(super) fn remove_event_listener(
        &mut self,
        target: ListenerTarget,
        arguments: &[Value],
    ) -> Result<(), ScriptError> {
        let event = match arguments.first() {
            Some(value) => self.coerce_string(value.clone())?,
            None => return Ok(()),
        };
        let callback = arguments.get(1).map(Value::resolve).unwrap_or(Value::Null);
        let capture = match arguments.get(2).map(Value::resolve) {
            Some(options) if !is_primitive(&options) => {
                self.get_property(&options, "capture")?.truthy()
            }
            Some(flag) => flag.truthy(),
            None => false,
        };
        if let Some(listeners) = self.event_listeners.get_mut(&target) {
            listeners.retain(|listener| {
                let matches = listener.event == event
                    && listener.capture == capture
                    && listener
                        .callback
                        .as_ref()
                        .is_some_and(|existing| values_equal(existing, &callback));
                if matches {
                    listener.removed.set(true);
                }
                !matches
            });
        }
        if let ListenerTarget::Object(id) = target
            && self.event_listeners.get(&target).is_none_or(Vec::is_empty)
        {
            self.held_targets.remove(&id);
        }
        Ok(())
    }

    // the on* handler property of a node or the window
    // the on* handler of a node or the window; a node without a script-set handler falls back
    // to its inline attribute
    pub(super) fn event_handler(&mut self, target: ListenerTarget, event: &str) -> Value {
        let node = match target {
            ListenerTarget::Window => {
                return self
                    .global
                    .borrow()
                    .get(&format!("on{event}"))
                    .filter(|value| !is_primitive(value))
                    .unwrap_or(Value::Null);
            }
            ListenerTarget::Node(node) => Some(node),
            ListenerTarget::Object(_) => {
                let handler = match self.target_value(target) {
                    Value::Object(object) => object
                        .borrow()
                        .get(&format!("on{event}"))
                        .cloned()
                        .unwrap_or(Value::Null),
                    _ => Value::Null,
                };
                return if is_primitive(&handler) {
                    Value::Null
                } else {
                    handler
                };
            }
        };
        if let Some(handler) = self.event_handlers.get(&(target, event.to_string())) {
            return handler.clone();
        }
        node.and_then(|node| self.attribute_handler(node, event))
            .unwrap_or(Value::Null)
    }

    pub(super) fn set_event_handler(&mut self, target: ListenerTarget, event: &str, value: Value) {
        let key = (target, event.to_string());
        if is_primitive(&value) {
            self.event_handlers.remove(&key);
            if let Some(listeners) = self.event_listeners.get_mut(&target) {
                listeners.retain(|listener| {
                    let slot = listener.callback.is_none() && listener.event == event;
                    if slot {
                        listener.removed.set(true);
                    }
                    !slot
                });
            }
            return;
        }
        // the handler keeps the listener position of its first assignment
        if self.event_handlers.insert(key, value).is_none() {
            self.event_listeners
                .entry(target)
                .or_default()
                .push(Listener {
                    event: event.to_string(),
                    callback: None,
                    capture: false,
                    once: false,
                    passive: false,
                    signal: None,
                    removed: Rc::new(Cell::new(false)),
                });
        }
    }

    // a handler from an inline on* attribute, compiled once per source text
    fn attribute_handler(&mut self, node: NodeId, event: &str) -> Option<Value> {
        let source = self
            .document
            .borrow()
            .attribute(node, &format!("on{event}"))?
            .to_string();
        let key = (node, event.to_string());
        if let Some((cached, handler)) = self.attribute_handlers.get(&key)
            && *cached == source
        {
            return (!matches!(handler, Value::Undefined)).then(|| handler.clone());
        }
        let check = self.content_security_policy().inline(
            crate::csp::Directive::ScriptAttr,
            &source,
            crate::csp::Initiator::default(),
        );
        if !self.permits(check) {
            // refused once per source text, as it would compile once
            self.attribute_handlers
                .insert(key, (source, Value::Undefined));
            return None;
        }
        let compiled = self.compile_function_text("event", &source);
        match compiled {
            Ok(handler) => {
                self.attribute_handlers
                    .insert(key, (source, handler.clone()));
                Some(handler)
            }
            Err(error) => {
                self.report_exception(error);
                None
            }
        }
    }

    // runs an event through its path; listener errors are reported, not thrown
    pub(super) fn dispatch(
        &mut self,
        event: &Rc<EventRecord>,
        target: ListenerTarget,
    ) -> Result<bool, ScriptError> {
        let _heap = super::heap::enter(self.heap_id);
        let event_type = event.event_type();
        let path = self.event_path(target, &event_type, event.composed);
        {
            let mut state = event.state.borrow_mut();
            state.dispatching = true;
            state.origin = match target {
                ListenerTarget::Node(node) if self.document.borrow().has_shadow_roots() => {
                    Some(node)
                }
                _ => None,
            };
            state.target = Some(self.target_value(target));
            state.path = path.iter().map(|&item| self.target_value(item)).collect();
        }
        let inhibit_gc = event.interface == "MediaQueryListEvent";
        if inhibit_gc {
            self.gc_inhibit += 1;
        }
        let root_base = self.temp_roots.len();
        self.temp_roots.push(Value::Event(event.clone()));
        let mut invoked = false;
        let outcome = (|| -> Result<bool, ScriptError> {
            for &item in path.iter().skip(1).rev() {
                if event.state.borrow().stop {
                    break;
                }
                invoked |= self.invoke_listeners(event, item, 1, true, &event_type)?;
            }
            // at the target, capturing listeners run before the others
            for capture in [true, false] {
                if !event.state.borrow().stop {
                    invoked |= self.invoke_listeners(event, target, 2, capture, &event_type)?;
                }
            }
            if event.meta.borrow().bubbles {
                for &item in path.iter().skip(1) {
                    if event.state.borrow().stop {
                        break;
                    }
                    invoked |= self.invoke_listeners(event, item, 3, false, &event_type)?;
                }
            }
            Ok(invoked)
        })();
        self.temp_roots.truncate(root_base);
        if inhibit_gc {
            self.gc_inhibit -= 1;
        }
        let mut state = event.state.borrow_mut();
        state.dispatching = false;
        state.phase = 0;
        state.current_target = None;
        state.stop = false;
        state.stop_immediate = false;
        state.path.clear();
        outcome
    }

    // the capturing or non-capturing listeners of one target, with the event in phase 1
    // (capturing), 2 (at target) or 3 (bubbling)
    fn invoke_listeners(
        &mut self,
        event: &Rc<EventRecord>,
        target: ListenerTarget,
        phase: u8,
        capture: bool,
        event_type: &str,
    ) -> Result<bool, ScriptError> {
        let current = self.target_value(target);
        let origin = event.state.borrow().origin;
        let retargeted = origin.map(|origin| self.node_value(self.retarget(origin, target)));
        {
            let mut state = event.state.borrow_mut();
            state.phase = phase;
            state.current_target = Some(current.clone());
            if let Some(retargeted) = retargeted {
                state.target = Some(retargeted);
            }
        }
        let mut invoked = false;
        // a window handler or an inline attribute handler that script never replaced runs
        // ahead of the listeners
        let implicit = !capture
            && match target {
                ListenerTarget::Window => true,
                ListenerTarget::Node(_) => !self
                    .event_handlers
                    .contains_key(&(target, event_type.to_string())),
                ListenerTarget::Object(_) => true,
            };
        if implicit {
            let handler = self.event_handler(target, event_type);
            if !matches!(handler, Value::Null) {
                invoked = true;
                let kind = if target == ListenerTarget::Window && event.interface == "ErrorEvent" {
                    CallbackKind::ErrorHandler
                } else {
                    CallbackKind::Handler
                };
                self.call_listener(event, &handler, &current, kind)?;
            }
        }
        let snapshot = self
            .event_listeners
            .get(&target)
            .map(|listeners| {
                listeners
                    .iter()
                    .filter(|listener| listener.event == event_type && listener.capture == capture)
                    .map(|listener| {
                        (
                            listener.callback.clone(),
                            listener.once,
                            listener.passive,
                            listener.removed.clone(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let root_base = self.temp_roots.len();
        self.temp_roots.push(current.clone());
        self.temp_roots
            .extend(snapshot.iter().filter_map(|(cb, ..)| cb.clone()));
        let mut outcome = Ok(invoked);
        for (callback, once, passive, removed) in snapshot {
            if removed.get() {
                continue;
            }
            if once {
                removed.set(true);
                if let Some(listeners) = self.event_listeners.get_mut(&target) {
                    listeners.retain(|listener| !Rc::ptr_eq(&listener.removed, &removed));
                }
            }
            let (callback, handler) = match callback {
                Some(callback) => (callback, false),
                None => (self.event_handler(target, event_type), true),
            };
            if matches!(callback, Value::Null | Value::Undefined) {
                continue;
            }
            invoked = true;
            event.state.borrow_mut().passive = passive;
            let kind = if handler {
                CallbackKind::Handler
            } else {
                CallbackKind::Listener
            };
            let step = self.call_listener(event, &callback, &current, kind);
            event.state.borrow_mut().passive = false;
            if let Err(error) = step {
                outcome = Err(error);
                break;
            }
            if event.state.borrow().stop_immediate {
                break;
            }
        }
        self.temp_roots.truncate(root_base);
        outcome.map(|_| invoked)
    }

    fn call_listener(
        &mut self,
        event: &Rc<EventRecord>,
        callback: &Value,
        current: &Value,
        kind: CallbackKind,
    ) -> Result<(), ScriptError> {
        let argument = Value::Event(event.clone());
        let prev_gc = self.gc_safepoint_allowed;
        self.gc_safepoint_allowed = self.nested_runs == 0 && self.gc_inhibit == 0;
        let outcome = if kind == CallbackKind::ErrorHandler && is_callable(callback) {
            let field = |name: &str, fallback: Value| event.field(name).unwrap_or(fallback);
            let arguments = vec![
                field("message", Value::String("".into())),
                field("filename", Value::String("".into())),
                field("lineno", Value::Number(0.0)),
                field("colno", Value::Number(0.0)),
                field("error", Value::Null),
            ];
            self.call_function(callback.clone(), current.clone(), arguments)
        } else if is_callable(callback) {
            self.call_function(callback.clone(), current.clone(), vec![argument])
        } else if kind != CallbackKind::Listener {
            Err(self.type_error("The event handler is not a function"))
        } else {
            match self.get_property(callback, "handleEvent") {
                Ok(method) if is_callable(&method) => {
                    self.call_function(method, callback.clone(), vec![argument])
                }
                Ok(_) => Err(self.type_error("The listener's handleEvent is not a function")),
                Err(error) => Err(error),
            }
        };
        self.gc_safepoint_allowed = prev_gc;
        match outcome {
            // window.onerror cancels by returning true, other handlers by returning false
            Ok(Value::Bool(true)) if kind == CallbackKind::ErrorHandler => self.cancel_event(event),
            Ok(Value::Bool(false)) if kind == CallbackKind::Handler => self.cancel_event(event),
            Ok(_) => {}
            Err(error) if self.interrupted => return Err(error),
            Err(error) => self.report_exception(error),
        }
        // a listener called from the host is the outermost script, so its microtasks run now
        if self.nested_runs == 0 {
            self.drain_microtasks()?;
        }
        Ok(())
    }

    fn cancel_event(&self, event: &EventRecord) {
        let cancelable = event.meta.borrow().cancelable;
        let mut state = event.state.borrow_mut();
        if cancelable && !state.passive {
            state.canceled = true;
        }
    }

    // fires a trusted event at a node or the window and reports what listeners did
    pub(super) fn fire(
        &mut self,
        target: ListenerTarget,
        event_type: &str,
        fields: HostEventFields,
    ) -> Result<EventDispatch, ScriptError> {
        let event = self.host_event(event_type, fields);
        let handled = self.dispatch(&event, target)?;
        Ok(EventDispatch {
            handled,
            default_prevented: event.canceled(),
        })
    }

    pub(super) fn call_event_member(
        &mut self,
        member: EventMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let (name, field) = *member.entry;
        let Value::Event(event) = this.resolve() else {
            return Err(self.type_error(format!(
                "Illegal invocation of '{name}' on a value that is not an Event"
            )));
        };
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        match (name, member.access) {
            ("preventDefault", EventAccess::Call) => {
                self.cancel_event(&event);
                Ok(Value::Undefined)
            }
            ("stopPropagation", EventAccess::Call) | ("cancelBubble", EventAccess::Set) => {
                if member.access == EventAccess::Call || first.truthy() {
                    event.state.borrow_mut().stop = true;
                }
                Ok(Value::Undefined)
            }
            ("stopImmediatePropagation", EventAccess::Call) => {
                let mut state = event.state.borrow_mut();
                state.stop = true;
                state.stop_immediate = true;
                Ok(Value::Undefined)
            }
            ("returnValue", EventAccess::Set) => {
                if !first.truthy() {
                    self.cancel_event(&event);
                }
                Ok(Value::Undefined)
            }
            ("composedPath", EventAccess::Call) => {
                Ok(Self::new_array(event.state.borrow().path.clone()))
            }
            ("initEvent", EventAccess::Call) | ("initCustomEvent", EventAccess::Call) => {
                if !event.state.borrow().dispatching {
                    let event_type = self.coerce_string(first)?;
                    let mut meta = event.meta.borrow_mut();
                    meta.event_type = event_type;
                    meta.bubbles = arguments.get(1).is_some_and(Value::truthy);
                    meta.cancelable = arguments.get(2).is_some_and(Value::truthy);
                    meta.initialized = true;
                    drop(meta);
                    event.state.borrow_mut().canceled = false;
                    if name == "initCustomEvent" {
                        let detail = arguments.get(3).cloned().unwrap_or(Value::Null);
                        event
                            .fields
                            .borrow_mut()
                            .insert("detail".to_string(), detail);
                    }
                }
                Ok(Value::Undefined)
            }
            ("getModifierState", EventAccess::Call) => {
                let key = self.coerce_string(first)?;
                let field = match key.as_str() {
                    "Control" => "ctrlKey",
                    "Shift" => "shiftKey",
                    "Alt" => "altKey",
                    "Meta" => "metaKey",
                    _ => return Ok(Value::Bool(false)),
                };
                Ok(Value::Bool(
                    event.field(field).is_some_and(|value| value.truthy()),
                ))
            }
            (_, EventAccess::Get) => Ok(match field {
                Core | CoreWritable => {
                    let meta = event.meta.borrow();
                    let state = event.state.borrow();
                    match name {
                        "type" => Value::String(meta.event_type.clone().into()),
                        "target" | "srcElement" => state.target.clone().unwrap_or(Value::Null),
                        "currentTarget" => state.current_target.clone().unwrap_or(Value::Null),
                        "eventPhase" => Value::Number(f64::from(state.phase)),
                        "bubbles" => Value::Bool(meta.bubbles),
                        "cancelable" => Value::Bool(meta.cancelable),
                        "defaultPrevented" => Value::Bool(state.canceled),
                        "returnValue" => Value::Bool(!state.canceled),
                        "cancelBubble" => Value::Bool(state.stop),
                        "composed" => Value::Bool(event.composed),
                        "isTrusted" => Value::Bool(event.trusted),
                        _ => Value::Number(event.time_stamp),
                    }
                }
                Number(default) => event.field(name).unwrap_or(Value::Number(default)),
                Flag => event.field(name).unwrap_or(Value::Bool(false)),
                Text => event
                    .field(name)
                    .unwrap_or_else(|| Value::String(String::new().into())),
                _ => event.field(name).unwrap_or(Value::Null),
            }),
            _ => Ok(Value::Undefined),
        }
    }

    pub(super) fn construct_event(
        &mut self,
        interface: &'static str,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let Some(event_type) = arguments.first().cloned() else {
            return Err(self.type_error(format!(
                "Failed to construct '{interface}': 1 argument required, but only 0 present."
            )));
        };
        let event_type = self.coerce_string(event_type)?;
        let init = arguments
            .get(1)
            .map(Value::resolve)
            .unwrap_or(Value::Undefined);
        let read = |runtime: &mut Self, name: &str| -> Result<Value, ScriptError> {
            if is_primitive(&init) {
                Ok(Value::Undefined)
            } else {
                runtime.get_property(&init, name)
            }
        };
        let bubbles = read(self, "bubbles")?.truthy();
        let cancelable = read(self, "cancelable")?.truthy();
        let composed = read(self, "composed")?.truthy();
        let event = self.new_event(interface, event_type, bubbles, cancelable, composed, false);
        // the interface chain's fields, read from the init dictionary when present
        let mut current = interface;
        while let Some(definition) = EVENT_INTERFACES
            .iter()
            .find(|candidate| candidate.name == current)
        {
            for &(name, field) in definition.members {
                if matches!(field, Core | CoreWritable | Method(_)) {
                    continue;
                }
                let value = read(self, name)?;
                if matches!(value, Value::Undefined) {
                    continue;
                }
                let value = match field {
                    Number(_) => Value::Number(self.coerce_number(value)?),
                    Flag => Value::Bool(value.truthy()),
                    Text => Value::String(self.coerce_string(value)?.into()),
                    _ => value,
                };
                event
                    .fields
                    .borrow_mut()
                    .entry(name.to_string())
                    .or_insert(value);
            }
            current = definition.parent;
        }
        if let Value::Object(_) = new_target {
            let prototype = self.prototype_from_constructor(&new_target)?;
            event.object.borrow_mut().set_prototype(Some(prototype));
        }
        Ok(Value::Event(event))
    }

    // EventTarget.prototype.dispatchEvent, for nodes and the window
    pub(super) fn dispatch_event_from_script(
        &mut self,
        target: ListenerTarget,
        event: &Value,
    ) -> Result<Value, ScriptError> {
        let Value::Event(event) = event.resolve() else {
            return Err(self.type_error(
                "Failed to execute 'dispatchEvent' on 'EventTarget': parameter 1 is not of type 'Event'.",
            ));
        };
        if event.state.borrow().dispatching || !event.meta.borrow().initialized {
            return Err(self.throw_value(dom_exception(
                "InvalidStateError",
                "The event is already being dispatched.",
            )));
        }
        self.dispatch(&event, target)?;
        Ok(Value::Bool(!event.canceled()))
    }

    // document.createEvent, the legacy way to make an event before initEvent
    pub(super) fn create_legacy_event(&mut self, interface: &str) -> Result<Value, ScriptError> {
        let name = match interface.to_ascii_lowercase().as_str() {
            "event" | "events" | "htmlevents" => "Event",
            "customevent" => "CustomEvent",
            "uievent" | "uievents" => "UIEvent",
            "mouseevent" | "mouseevents" => "MouseEvent",
            "keyboardevent" => "KeyboardEvent",
            "focusevent" => "FocusEvent",
            "messageevent" => "MessageEvent",
            _ => {
                return Err(self.throw_value(dom_exception(
                    "NotSupportedError",
                    &format!("The provided event type ('{interface}') is invalid."),
                )));
            }
        };
        let event = self.new_event(name, String::new(), false, false, false, false);
        event.meta.borrow_mut().initialized = false;
        Ok(Value::Event(event))
    }

    // the error event at the global object for an uncaught exception; true when script
    // canceled it, which keeps it out of the console
    pub(super) fn report_error_event(&mut self, text: &str, error: Value) -> bool {
        if self.reporting_error || self.interrupted {
            return false;
        }
        self.reporting_error = true;
        let fields = HostEventFields {
            message: Some(format!("Uncaught {text}")),
            error: Some(error),
            ..HostEventFields::default()
        };
        let dispatch = self.fire(ListenerTarget::Window, "error", fields);
        self.reporting_error = false;
        dispatch.is_ok_and(|dispatch| dispatch.default_prevented)
    }

    // an event at a target outside the node tree, such as a Worker or an AbortSignal, which
    // keeps its own callbacks
    pub(super) fn deliver(
        &mut self,
        event: &Rc<EventRecord>,
        target: Value,
        callbacks: Vec<Value>,
    ) -> Result<(), ScriptError> {
        {
            let mut state = event.state.borrow_mut();
            state.dispatching = true;
            state.phase = 2;
            state.target = Some(target.clone());
            state.current_target = Some(target.clone());
            state.path = vec![target.clone()];
        }
        for callback in callbacks {
            self.call_listener(event, &callback, &target, CallbackKind::Listener)?;
            if event.state.borrow().stop_immediate {
                break;
            }
        }
        let mut state = event.state.borrow_mut();
        state.dispatching = false;
        state.phase = 0;
        state.current_target = None;
        state.path.clear();
        Ok(())
    }

    // blur then focusout, or focus then focusin, with the other element as relatedTarget
    pub(super) fn fire_focus_change(
        &mut self,
        node: NodeId,
        event_type: &str,
        related: Option<NodeId>,
    ) -> Result<(), ScriptError> {
        let paired = if event_type == "blur" {
            "focusout"
        } else {
            "focusin"
        };
        for name in [event_type, paired] {
            let fields = HostEventFields {
                related_target: Some(self.optional_node_value(related)),
                ..HostEventFields::default()
            };
            self.fire(ListenerTarget::Node(node), name, fields)?;
        }
        self.drain_microtasks()
    }

    fn optional_node_value(&self, node: Option<NodeId>) -> Value {
        node.filter(|&node| self.document.borrow().contains(node))
            .map_or(Value::Null, |node| self.node_value(node))
    }

    pub(super) fn remove_signal_listeners(&mut self, signal: &AbortSignalHandle) {
        self.event_listeners.retain(|_, listeners| {
            listeners.retain(|listener| {
                let aborted = listener
                    .signal
                    .as_ref()
                    .is_some_and(|candidate| Rc::ptr_eq(candidate, signal));
                if aborted {
                    listener.removed.set(true);
                }
                !aborted
            });
            !listeners.is_empty()
        });
    }

    pub(crate) fn set_pointer_position(&mut self, x: f32, y: f32) {
        self.pointer.x = x;
        self.pointer.y = y;
    }

    // a press or release of button 0 (primary), 1 (auxiliary) or 2 (secondary)
    pub(crate) fn set_pointer_button(&mut self, button: i16, pressed: bool) {
        let bit = match button {
            0 => 1,
            1 => 4,
            2 => 2,
            _ => 0,
        };
        self.pointer.button = button;
        if pressed {
            self.note_activation();
            self.pointer.buttons |= bit;
        } else {
            self.pointer.buttons &= !bit;
        }
    }

    pub(crate) fn set_pointer_modifiers(&mut self, modifiers: KeyModifiers) {
        self.pointer.modifiers = modifiers;
    }

    pub(crate) fn dispatch_wheel_event(
        &mut self,
        node: NodeId,
        delta_x: f32,
        delta_y: f32,
    ) -> Result<(bool, bool), ScriptError> {
        self.pointer.wheel_delta = (delta_x, delta_y);
        let dispatch = self.fire(
            ListenerTarget::Node(node),
            "wheel",
            HostEventFields::default(),
        );
        self.pointer.wheel_delta = (0.0, 0.0);
        let dispatch = dispatch?;
        self.drain_microtasks()?;
        Ok((dispatch.handled, dispatch.default_prevented))
    }

    // a pointer event followed by its compatibility mouse event
    pub(crate) fn dispatch_pointer_event(
        &mut self,
        node: NodeId,
        event_type: &str,
    ) -> Result<bool, ScriptError> {
        let target = ListenerTarget::Node(node);
        match event_type {
            "pointerdown" if self.pointer.button == 0 => self.active_node = Some(node),
            "pointerup" => self.active_node = None,
            _ => {}
        }
        let pointer = self.fire(target, event_type, HostEventFields::default())?;
        // a release ends any capture once its pointerup has gone out
        if event_type == "pointerup" {
            self.pointer.released_capture = self.pointer.capture;
            self.pointer.pending_capture = None;
            self.process_pointer_capture()?;
        }
        let mouse = match event_type {
            "pointerdown" => {
                self.pointer.suppress_mouse = pointer.default_prevented;
                Some("mousedown")
            }
            "pointermove" => Some("mousemove"),
            "pointerup" => Some("mouseup"),
            _ => None,
        };
        let mut handled = pointer.handled;
        if let Some(mouse) = mouse
            && !self.pointer.suppress_mouse
            && self.document.borrow().is_connected(node)
        {
            handled |= self
                .fire(target, mouse, HostEventFields::default())?
                .handled;
        }
        if event_type == "pointerup" {
            self.pointer.suppress_mouse = false;
        }
        self.drain_microtasks()?;
        Ok(handled)
    }

    // where a pointer event goes: the capturing element, else the one under the pointer
    pub(crate) fn pointer_target(
        &mut self,
        hit: Option<NodeId>,
    ) -> Result<Option<NodeId>, ScriptError> {
        self.process_pointer_capture()?;
        Ok(self.pointer.capture.or(hit))
    }

    // a click after a captured release goes to the element that had the capture
    pub(crate) fn click_target(&self, hit: Option<NodeId>) -> Option<NodeId> {
        let document = self.document.borrow();
        self.pointer
            .released_capture
            .filter(|&node| document.is_connected(node))
            .or(hit)
    }

    // hands capture over to what script last asked for, with lost and got events
    fn process_pointer_capture(&mut self) -> Result<(), ScriptError> {
        let connected = |runtime: &Self, node: NodeId| runtime.document.borrow().is_connected(node);
        if self
            .pointer
            .pending_capture
            .is_some_and(|node| !connected(self, node))
        {
            self.pointer.pending_capture = None;
        }
        if self.pointer.capture == self.pointer.pending_capture {
            return Ok(());
        }
        let lost = std::mem::replace(&mut self.pointer.capture, self.pointer.pending_capture);
        if let Some(lost) = lost {
            // an element that left the document hands its lost event to the document
            let node = if connected(self, lost) {
                lost
            } else {
                self.document_node()
            };
            self.fire(
                ListenerTarget::Node(node),
                "lostpointercapture",
                HostEventFields::default(),
            )?;
        }
        if let Some(got) = self.pointer.capture {
            self.fire(
                ListenerTarget::Node(got),
                "gotpointercapture",
                HostEventFields::default(),
            )?;
        }
        Ok(())
    }

    pub(super) fn pointer_capture_member(
        &mut self,
        node: NodeId,
        name: &str,
        pointer: Value,
    ) -> Result<Value, ScriptError> {
        // the mouse is the only pointer, and its id is 1 as in browsers
        if self.coerce_number(pointer)? != 1.0 {
            return Err(self.throw_value(dom_exception(
                "NotFoundError",
                "No active pointer with the given id is found.",
            )));
        }
        let held = self.pointer.pending_capture == Some(node);
        match name {
            "hasPointerCapture" => return Ok(Value::Bool(held)),
            "releasePointerCapture" if held => self.pointer.pending_capture = None,
            "setPointerCapture" if !self.document.borrow().is_connected(node) => {
                return Err(self.throw_value(dom_exception(
                    "InvalidStateError",
                    "The element is not in a document.",
                )));
            }
            // capture only holds while a button is down
            "setPointerCapture" if self.pointer.buttons != 0 => {
                self.pointer.pending_capture = Some(node);
            }
            _ => {}
        }
        Ok(Value::Undefined)
    }

    // the dragstart of a press that moved far enough on a draggable element
    pub(crate) fn dispatch_drag_start(&mut self, node: NodeId) -> Result<bool, ScriptError> {
        if !self.document.borrow().is_connected(node) {
            return Ok(false);
        }
        let started = self.fire(
            ListenerTarget::Node(node),
            "dragstart",
            HostEventFields::default(),
        )?;
        self.drain_microtasks()?;
        Ok(started.handled)
    }

    // out, leave, over and enter events as the pointer moves from one node to another
    pub(crate) fn dispatch_hover_change(
        &mut self,
        previous: Option<NodeId>,
        next: Option<NodeId>,
    ) -> Result<bool, ScriptError> {
        self.hovered_node = next;
        let (previous, next, left, entered) = {
            let document = self.document.borrow();
            let live = |node: Option<NodeId>| node.filter(|&node| document.is_connected(node));
            let chain = |node: Option<NodeId>| -> Vec<NodeId> {
                node.map(|node| {
                    std::iter::once(node)
                        .chain(document.ancestors(node))
                        .filter(|&node| document.element(node).is_some())
                        .collect()
                })
                .unwrap_or_default()
            };
            let (previous, next) = (live(previous), live(next));
            (previous, next, chain(previous), chain(next))
        };
        let mut handled = false;
        let mut fire = |runtime: &mut Self, node: NodeId, names: [&str; 2], related| {
            for name in names {
                let fields = HostEventFields {
                    related_target: Some(runtime.optional_node_value(related)),
                    ..HostEventFields::default()
                };
                handled |= runtime
                    .fire(ListenerTarget::Node(node), name, fields)?
                    .handled;
            }
            Ok::<(), ScriptError>(())
        };
        if let Some(previous) = previous {
            fire(self, previous, ["pointerout", "mouseout"], next)?;
            for &node in left.iter().filter(|node| !entered.contains(node)) {
                fire(self, node, ["pointerleave", "mouseleave"], next)?;
            }
        }
        if let Some(next) = next {
            fire(self, next, ["pointerover", "mouseover"], previous)?;
            for &node in entered.iter().rev().filter(|node| !left.contains(node)) {
                fire(self, node, ["pointerenter", "mouseenter"], previous)?;
            }
        }
        self.drain_microtasks()?;
        Ok(handled)
    }

    // a keydown with the physical code and repeat flag the host saw
    pub(crate) fn key_down_with_code(
        &mut self,
        key: &str,
        code: &str,
        modifiers: KeyModifiers,
        repeat: bool,
    ) -> Result<bool, ScriptError> {
        self.key_detail = Some((code.to_string(), repeat));
        // keys that only modify another, and escape, do not activate the page
        if !matches!(
            key,
            "Escape" | "Shift" | "Control" | "Alt" | "Meta" | "AltGraph" | "CapsLock"
        ) {
            self.note_activation();
        }
        let handled = self.key_down_with_modifiers(key, modifiers);
        self.key_detail = None;
        handled
    }

    // a keyup at the focused element, or the body when nothing has focus
    pub(crate) fn key_up(
        &mut self,
        key: &str,
        code: &str,
        modifiers: KeyModifiers,
    ) -> Result<bool, ScriptError> {
        self.key_text_suppressed = false;
        let Some(target) = self.key_target() else {
            return Ok(false);
        };
        let fields = HostEventFields {
            key: key.to_string(),
            modifiers,
            ..HostEventFields::default()
        };
        self.key_detail = Some((code.to_string(), false));
        let dispatch = self.fire(ListenerTarget::Node(target), "keyup", fields);
        self.key_detail = None;
        let dispatch = dispatch?;
        self.drain_microtasks()?;
        Ok(dispatch.handled)
    }
}
