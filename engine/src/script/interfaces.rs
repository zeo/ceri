// the DOM interfaces as prototype objects: every node gets a wrapper object whose prototype
// chain runs through its interfaces, so instanceof, `name in element`, prototype patching and
// expando properties behave as pages expect

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::realm::{Builder, Native};
use super::*;
use crate::dom::{Namespace, NodeKind};
use std::cmp::Ordering as TreeOrdering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Access {
    Get,
    Set,
    Call,
}

// one property of one interface, carried by the native function that implements it; the
// entry points into the static tables so the native stays two words wide
#[derive(Clone, Copy, Debug)]
pub(super) struct DomMember {
    entry: &'static (&'static str, Kind),
    pub(super) access: Access,
}

impl DomMember {
    fn name(self) -> &'static str {
        self.entry.0
    }

    fn reflect(self) -> Reflect {
        match self.entry.1 {
            Attr(attribute) => Reflect::Text(attribute),
            Flag(attribute) => Reflect::Flag(attribute),
            _ => Reflect::None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reflect {
    None,
    Text(&'static str),
    Flag(&'static str),
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    ReadOnly,
    ReadWrite,
    Method(u32),
    Reflect(&'static str),
    Flag(&'static str),
    // an on* event handler property
    Handler,
}

use Kind::{Flag, Handler, Method, ReadOnly, ReadWrite, Reflect as Attr};

struct Interface {
    name: &'static str,
    parent: &'static str,
    members: &'static [(&'static str, Kind)],
}

static FORM_VALIDATION: &[(&str, Kind)] = &[
    ("form", ReadOnly),
    ("validity", ReadOnly),
    ("willValidate", ReadOnly),
    ("validationMessage", ReadOnly),
    ("checkValidity", Method(0)),
    ("reportValidity", Method(0)),
    ("setCustomValidity", Method(1)),
];

// the GlobalEventHandlers mixin of HTML and SVG elements and the document
static EVENT_HANDLERS: &[(&str, Kind)] = &[
    ("onabort", Handler),
    ("onanimationend", Handler),
    ("onanimationiteration", Handler),
    ("onanimationstart", Handler),
    ("onauxclick", Handler),
    ("onbeforeinput", Handler),
    ("onblur", Handler),
    ("oncancel", Handler),
    ("oncanplay", Handler),
    ("oncanplaythrough", Handler),
    ("onchange", Handler),
    ("onclick", Handler),
    ("onclose", Handler),
    ("oncontextmenu", Handler),
    ("oncopy", Handler),
    ("oncut", Handler),
    ("ondblclick", Handler),
    ("ondurationchange", Handler),
    ("onemptied", Handler),
    ("onended", Handler),
    ("onerror", Handler),
    ("onfocus", Handler),
    ("onfocusin", Handler),
    ("onfocusout", Handler),
    ("oninput", Handler),
    ("oninvalid", Handler),
    ("onkeydown", Handler),
    ("onkeypress", Handler),
    ("onkeyup", Handler),
    ("onload", Handler),
    ("onloadeddata", Handler),
    ("onloadedmetadata", Handler),
    ("onloadstart", Handler),
    ("onmousedown", Handler),
    ("onmouseenter", Handler),
    ("onmouseleave", Handler),
    ("onmousemove", Handler),
    ("onmouseout", Handler),
    ("onmouseover", Handler),
    ("onmouseup", Handler),
    ("onpaste", Handler),
    ("onpause", Handler),
    ("onplay", Handler),
    ("onplaying", Handler),
    ("onpointercancel", Handler),
    ("onpointerdown", Handler),
    ("onpointerenter", Handler),
    ("onpointerleave", Handler),
    ("onpointermove", Handler),
    ("onpointerout", Handler),
    ("onpointerover", Handler),
    ("onpointerup", Handler),
    ("onprogress", Handler),
    ("onratechange", Handler),
    ("onreset", Handler),
    ("onresize", Handler),
    ("onscroll", Handler),
    ("onseeked", Handler),
    ("onseeking", Handler),
    ("onselect", Handler),
    ("onselectionchange", Handler),
    ("onstalled", Handler),
    ("onsubmit", Handler),
    ("onsuspend", Handler),
    ("ontimeupdate", Handler),
    ("ontoggle", Handler),
    ("ontransitioncancel", Handler),
    ("ontransitionend", Handler),
    ("ontransitionrun", Handler),
    ("ontransitionstart", Handler),
    ("onvolumechange", Handler),
    ("onwaiting", Handler),
    ("onwheel", Handler),
];
const HANDLING: &[&str] = &["Document", "HTMLElement", "SVGElement"];

static TEXT_SELECTION: &[(&str, Kind)] = &[
    ("selectionStart", ReadWrite),
    ("selectionEnd", ReadWrite),
    ("selectionDirection", ReadWrite),
    ("select", Method(0)),
    ("setSelectionRange", Method(2)),
    ("setRangeText", Method(1)),
];

// parents come before their children
static INTERFACES: &[Interface] = &[
    Interface {
        name: "EventTarget",
        parent: "",
        members: &[
            ("addEventListener", Method(2)),
            ("removeEventListener", Method(2)),
            ("dispatchEvent", Method(1)),
        ],
    },
    Interface {
        name: "Node",
        parent: "EventTarget",
        members: &[
            ("nodeType", ReadOnly),
            ("nodeName", ReadOnly),
            ("nodeValue", ReadWrite),
            ("textContent", ReadWrite),
            ("parentNode", ReadOnly),
            ("parentElement", ReadOnly),
            ("childNodes", ReadOnly),
            ("firstChild", ReadOnly),
            ("lastChild", ReadOnly),
            ("previousSibling", ReadOnly),
            ("nextSibling", ReadOnly),
            ("isConnected", ReadOnly),
            ("ownerDocument", ReadOnly),
            ("hasChildNodes", Method(0)),
            ("cloneNode", Method(0)),
            ("normalize", Method(0)),
            ("appendChild", Method(1)),
            ("insertBefore", Method(2)),
            ("removeChild", Method(1)),
            ("replaceChild", Method(2)),
            ("contains", Method(1)),
            ("getRootNode", Method(0)),
            ("compareDocumentPosition", Method(1)),
            ("isSameNode", Method(1)),
            ("isEqualNode", Method(1)),
            ("lookupNamespaceURI", Method(1)),
            ("baseURI", ReadOnly),
        ],
    },
    Interface {
        name: "Element",
        parent: "Node",
        members: &[
            ("requestFullscreen", Method(0)),
            ("tagName", ReadOnly),
            ("localName", ReadOnly),
            ("namespaceURI", ReadOnly),
            ("id", ReadWrite),
            ("className", ReadWrite),
            ("classList", ReadOnly),
            ("attributes", ReadOnly),
            ("attributeStyleMap", ReadOnly),
            ("computedStyleMap", Method(0)),
            ("innerHTML", ReadWrite),
            ("outerHTML", ReadWrite),
            ("children", ReadOnly),
            ("firstElementChild", ReadOnly),
            ("lastElementChild", ReadOnly),
            ("childElementCount", ReadOnly),
            ("previousElementSibling", ReadOnly),
            ("nextElementSibling", ReadOnly),
            ("scrollTop", ReadWrite),
            ("scrollLeft", ReadWrite),
            ("slot", Attr("slot")),
            ("getAttribute", Method(1)),
            ("setAttribute", Method(2)),
            ("removeAttribute", Method(1)),
            ("hasAttribute", Method(1)),
            ("toggleAttribute", Method(1)),
            ("getAttributeNames", Method(0)),
            ("attachShadow", Method(1)),
            ("shadowRoot", ReadOnly),
            ("assignedSlot", ReadOnly),
            ("slot", Attr("slot")),
            ("getBoundingClientRect", Method(0)),
            ("getClientRects", Method(0)),
            ("animate", Method(1)),
            ("getAnimations", Method(0)),
            ("checkVisibility", Method(0)),
            ("scrollIntoView", Method(0)),
            ("scroll", Method(0)),
            ("scrollTo", Method(0)),
            ("scrollBy", Method(0)),
            ("setPointerCapture", Method(1)),
            ("releasePointerCapture", Method(1)),
            ("hasPointerCapture", Method(1)),
            ("clientWidth", ReadOnly),
            ("clientHeight", ReadOnly),
            ("clientTop", ReadOnly),
            ("clientLeft", ReadOnly),
            ("scrollWidth", ReadOnly),
            ("scrollHeight", ReadOnly),
            ("hasAttributes", Method(0)),
            ("querySelector", Method(1)),
            ("querySelectorAll", Method(1)),
            ("closest", Method(1)),
            ("matches", Method(1)),
            ("getElementsByTagName", Method(1)),
            ("getElementsByClassName", Method(1)),
            ("append", Method(0)),
            ("prepend", Method(0)),
            ("replaceChildren", Method(0)),
            ("before", Method(0)),
            ("after", Method(0)),
            ("replaceWith", Method(0)),
            ("remove", Method(0)),
            ("insertAdjacentHTML", Method(2)),
            ("setHTMLUnsafe", Method(1)),
            ("insertAdjacentElement", Method(2)),
            ("insertAdjacentText", Method(2)),
            ("webkitMatchesSelector", Method(1)),
            ("getAttributeNS", Method(2)),
            ("setAttributeNS", Method(3)),
            ("removeAttributeNS", Method(2)),
            ("hasAttributeNS", Method(2)),
            ("getHTML", Method(0)),
            ("role", ReadWrite),
            ("ariaAtomic", ReadWrite),
            ("ariaAutoComplete", ReadWrite),
            ("ariaBusy", ReadWrite),
            ("ariaChecked", ReadWrite),
            ("ariaColCount", ReadWrite),
            ("ariaColIndex", ReadWrite),
            ("ariaColSpan", ReadWrite),
            ("ariaCurrent", ReadWrite),
            ("ariaDescription", ReadWrite),
            ("ariaDisabled", ReadWrite),
            ("ariaExpanded", ReadWrite),
            ("ariaHasPopup", ReadWrite),
            ("ariaHidden", ReadWrite),
            ("ariaInvalid", ReadWrite),
            ("ariaKeyShortcuts", ReadWrite),
            ("ariaLabel", ReadWrite),
            ("ariaLevel", ReadWrite),
            ("ariaLive", ReadWrite),
            ("ariaModal", ReadWrite),
            ("ariaMultiLine", ReadWrite),
            ("ariaMultiSelectable", ReadWrite),
            ("ariaOrientation", ReadWrite),
            ("ariaPlaceholder", ReadWrite),
            ("ariaPosInSet", ReadWrite),
            ("ariaPressed", ReadWrite),
            ("ariaReadOnly", ReadWrite),
            ("ariaRelevant", ReadWrite),
            ("ariaRequired", ReadWrite),
            ("ariaRoleDescription", ReadWrite),
            ("ariaRowCount", ReadWrite),
            ("ariaRowIndex", ReadWrite),
            ("ariaRowSpan", ReadWrite),
            ("ariaSelected", ReadWrite),
            ("ariaSetSize", ReadWrite),
            ("ariaSort", ReadWrite),
            ("ariaValueMax", ReadWrite),
            ("ariaValueMin", ReadWrite),
            ("ariaValueNow", ReadWrite),
            ("ariaValueText", ReadWrite),
        ],
    },
    Interface {
        name: "CharacterData",
        parent: "Node",
        members: &[
            ("data", ReadWrite),
            ("length", ReadOnly),
            ("previousElementSibling", ReadOnly),
            ("nextElementSibling", ReadOnly),
            ("substringData", Method(2)),
            ("appendData", Method(1)),
            ("insertData", Method(2)),
            ("deleteData", Method(2)),
            ("replaceData", Method(3)),
            ("before", Method(0)),
            ("after", Method(0)),
            ("replaceWith", Method(0)),
            ("remove", Method(0)),
        ],
    },
    Interface {
        name: "Text",
        parent: "CharacterData",
        members: &[("splitText", Method(1)), ("wholeText", ReadOnly)],
    },
    Interface {
        name: "Comment",
        parent: "CharacterData",
        members: &[],
    },
    Interface {
        name: "DocumentFragment",
        parent: "Node",
        members: &[
            ("children", ReadOnly),
            ("firstElementChild", ReadOnly),
            ("lastElementChild", ReadOnly),
            ("childElementCount", ReadOnly),
            ("getElementById", Method(1)),
            ("querySelector", Method(1)),
            ("querySelectorAll", Method(1)),
            ("append", Method(0)),
            ("prepend", Method(0)),
            ("replaceChildren", Method(0)),
        ],
    },
    Interface {
        name: "ShadowRoot",
        parent: "DocumentFragment",
        members: &[
            ("host", ReadOnly),
            ("mode", ReadOnly),
            ("innerHTML", ReadWrite),
            ("setHTMLUnsafe", Method(1)),
            ("styleSheets", ReadOnly),
            ("adoptedStyleSheets", ReadWrite),
        ],
    },
    Interface {
        name: "Document",
        parent: "Node",
        members: &[
            ("documentElement", ReadOnly),
            ("body", ReadOnly),
            ("head", ReadOnly),
            ("title", ReadWrite),
            ("activeElement", ReadOnly),
            ("defaultView", ReadOnly),
            ("readyState", ReadOnly),
            ("visibilityState", ReadOnly),
            ("hidden", ReadOnly),
            ("styleSheets", ReadOnly),
            ("adoptedStyleSheets", ReadWrite),
            ("fonts", ReadOnly),
            ("fullscreenElement", ReadOnly),
            ("fullscreenEnabled", ReadOnly),
            ("fullscreen", ReadOnly),
            ("exitFullscreen", Method(0)),
            ("children", ReadOnly),
            ("firstElementChild", ReadOnly),
            ("lastElementChild", ReadOnly),
            ("childElementCount", ReadOnly),
            ("getElementById", Method(1)),
            ("getElementsByTagName", Method(1)),
            ("getElementsByClassName", Method(1)),
            ("querySelector", Method(1)),
            ("querySelectorAll", Method(1)),
            ("createElement", Method(1)),
            ("createElementNS", Method(2)),
            ("createTextNode", Method(1)),
            ("createComment", Method(1)),
            ("createDocumentFragment", Method(0)),
            ("importNode", Method(1)),
            ("adoptNode", Method(1)),
            ("createEvent", Method(1)),
            ("startViewTransition", Method(0)),
            ("execCommand", Method(1)),
            ("queryCommandSupported", Method(1)),
            ("queryCommandEnabled", Method(1)),
            ("queryCommandState", Method(1)),
            ("queryCommandValue", Method(1)),
            ("activeViewTransition", ReadOnly),
            ("currentScript", ReadOnly),
            ("createRange", Method(0)),
            ("createTreeWalker", Method(1)),
            ("getAnimations", Method(0)),
            ("createNodeIterator", Method(1)),
            ("evaluate", Method(2)),
            ("createExpression", Method(1)),
            ("createNSResolver", Method(1)),
            ("getSelection", Method(0)),
            ("elementFromPoint", Method(2)),
            ("elementsFromPoint", Method(2)),
            ("append", Method(0)),
            ("prepend", Method(0)),
            ("replaceChildren", Method(0)),
            ("URL", ReadOnly),
            ("documentURI", ReadOnly),
            ("characterSet", ReadOnly),
            ("charset", ReadOnly),
            ("inputEncoding", ReadOnly),
            ("contentType", ReadOnly),
            ("compatMode", ReadOnly),
            ("referrer", ReadOnly),
            ("dir", ReadWrite),
            ("cookie", ReadWrite),
            ("location", ReadOnly),
            ("scrollingElement", ReadOnly),
            ("forms", ReadOnly),
            ("images", ReadOnly),
            ("links", ReadOnly),
            ("scripts", ReadOnly),
            ("embeds", ReadOnly),
            ("plugins", ReadOnly),
            ("anchors", ReadOnly),
            ("getElementsByName", Method(1)),
            ("hasFocus", Method(0)),
        ],
    },
    Interface {
        name: "HTMLElement",
        parent: "Element",
        members: &[
            ("style", ReadOnly),
            ("dataset", ReadOnly),
            ("innerText", ReadWrite),
            ("offsetWidth", ReadOnly),
            ("offsetHeight", ReadOnly),
            ("offsetTop", ReadOnly),
            ("offsetLeft", ReadOnly),
            ("offsetParent", ReadOnly),
            ("hidden", Flag("hidden")),
            ("title", Attr("title")),
            ("lang", Attr("lang")),
            ("dir", Attr("dir")),
            ("accessKey", Attr("accesskey")),
            ("inputMode", Attr("inputmode")),
            ("enterKeyHint", Attr("enterkeyhint")),
            ("autofocus", Flag("autofocus")),
            ("inert", Flag("inert")),
            ("tabIndex", ReadWrite),
            ("draggable", ReadWrite),
            ("spellcheck", ReadWrite),
            ("translate", ReadWrite),
            ("contentEditable", ReadWrite),
            ("isContentEditable", ReadOnly),
            ("click", Method(0)),
            ("focus", Method(0)),
            ("blur", Method(0)),
        ],
    },
    Interface {
        name: "SVGElement",
        parent: "Element",
        members: &[("style", ReadOnly), ("dataset", ReadOnly)],
    },
    Interface {
        name: "SVGGraphicsElement",
        parent: "SVGElement",
        members: &[("getBBox", Method(0))],
    },
    Interface {
        name: "SVGSVGElement",
        parent: "SVGGraphicsElement",
        members: &[],
    },
    Interface {
        name: "HTMLInputElement",
        parent: "HTMLElement",
        members: &[
            ("value", ReadWrite),
            ("valueAsNumber", ReadWrite),
            ("defaultValue", ReadWrite),
            ("checked", ReadWrite),
            ("defaultChecked", ReadWrite),
            ("disabled", ReadWrite),
            ("readOnly", ReadWrite),
            ("required", ReadWrite),
            ("maxLength", ReadWrite),
            ("minLength", ReadWrite),
            ("files", ReadOnly),
            ("type", Attr("type")),
            ("name", Attr("name")),
            ("placeholder", Attr("placeholder")),
            ("min", Attr("min")),
            ("max", Attr("max")),
            ("step", Attr("step")),
            ("pattern", Attr("pattern")),
            ("accept", Attr("accept")),
            ("autocomplete", Attr("autocomplete")),
            ("multiple", Flag("multiple")),
            ("autofocus", Flag("autofocus")),
        ],
    },
    Interface {
        name: "HTMLTextAreaElement",
        parent: "HTMLElement",
        members: &[
            ("value", ReadWrite),
            ("defaultValue", ReadWrite),
            ("disabled", ReadWrite),
            ("readOnly", ReadWrite),
            ("required", ReadWrite),
            ("maxLength", ReadWrite),
            ("minLength", ReadWrite),
            ("name", Attr("name")),
            ("placeholder", Attr("placeholder")),
            ("wrap", Attr("wrap")),
            ("autofocus", Flag("autofocus")),
        ],
    },
    Interface {
        name: "HTMLSelectElement",
        parent: "HTMLElement",
        members: &[
            ("value", ReadWrite),
            ("selectedIndex", ReadWrite),
            ("multiple", ReadWrite),
            ("options", ReadOnly),
            ("selectedOptions", ReadOnly),
            ("disabled", ReadWrite),
            ("required", ReadWrite),
            ("name", Attr("name")),
            ("autofocus", Flag("autofocus")),
        ],
    },
    Interface {
        name: "HTMLOptionElement",
        parent: "HTMLElement",
        members: &[
            ("value", ReadWrite),
            ("selected", ReadWrite),
            ("defaultSelected", ReadWrite),
            ("index", ReadOnly),
            ("text", ReadWrite),
            ("form", ReadOnly),
            ("label", Attr("label")),
            ("disabled", Flag("disabled")),
        ],
    },
    Interface {
        name: "HTMLButtonElement",
        parent: "HTMLElement",
        members: &[
            ("value", ReadWrite),
            ("disabled", ReadWrite),
            ("name", Attr("name")),
            ("type", Attr("type")),
            ("autofocus", Flag("autofocus")),
        ],
    },
    Interface {
        name: "HTMLFormElement",
        parent: "HTMLElement",
        members: &[
            ("elements", ReadOnly),
            ("requestSubmit", Method(0)),
            ("reset", Method(0)),
            ("checkValidity", Method(0)),
            ("reportValidity", Method(0)),
            ("action", Attr("action")),
            ("method", Attr("method")),
            ("name", Attr("name")),
            ("noValidate", Flag("novalidate")),
        ],
    },
    Interface {
        name: "HTMLCanvasElement",
        parent: "HTMLElement",
        members: &[
            ("width", ReadWrite),
            ("height", ReadWrite),
            ("getContext", Method(1)),
            ("toDataURL", Method(0)),
            ("toBlob", Method(1)),
        ],
    },
    Interface {
        name: "HTMLDialogElement",
        parent: "HTMLElement",
        members: &[
            ("open", Flag("open")),
            ("returnValue", ReadWrite),
            ("show", Method(0)),
            ("showModal", Method(0)),
            ("close", Method(0)),
            ("requestClose", Method(0)),
        ],
    },
    Interface {
        name: "HTMLTemplateElement",
        parent: "HTMLElement",
        members: &[("content", ReadOnly)],
    },
    Interface {
        name: "HTMLAnchorElement",
        parent: "HTMLElement",
        members: &[
            ("href", Attr("href")),
            ("target", Attr("target")),
            ("rel", Attr("rel")),
            ("download", Attr("download")),
        ],
    },
    Interface {
        name: "HTMLImageElement",
        parent: "HTMLElement",
        members: &[
            ("src", Attr("src")),
            ("alt", Attr("alt")),
            ("srcset", Attr("srcset")),
            ("sizes", Attr("sizes")),
            ("crossOrigin", ReadWrite),
            ("decoding", ReadWrite),
            ("loading", ReadWrite),
            ("naturalWidth", ReadOnly),
            ("naturalHeight", ReadOnly),
            ("complete", ReadOnly),
            ("currentSrc", ReadOnly),
            ("decode", Method(0)),
        ],
    },
    Interface {
        name: "HTMLMediaElement",
        parent: "HTMLElement",
        members: &[
            ("src", Attr("src")),
            ("currentSrc", ReadOnly),
            ("crossOrigin", ReadWrite),
            ("networkState", ReadOnly),
            ("preload", Attr("preload")),
            ("load", Method(0)),
            ("canPlayType", Method(1)),
            ("readyState", ReadOnly),
            ("seeking", ReadOnly),
            ("currentTime", ReadWrite),
            ("fastSeek", Method(1)),
            ("duration", ReadOnly),
            ("paused", ReadOnly),
            ("defaultPlaybackRate", ReadWrite),
            ("playbackRate", ReadWrite),
            ("preservesPitch", ReadWrite),
            ("ended", ReadOnly),
            ("autoplay", Flag("autoplay")),
            ("loop", Flag("loop")),
            ("play", Method(0)),
            ("pause", Method(0)),
            ("controls", Flag("controls")),
            ("volume", ReadWrite),
            ("muted", ReadWrite),
            ("defaultMuted", Flag("muted")),
            ("error", ReadOnly),
        ],
    },
    Interface {
        name: "HTMLVideoElement",
        parent: "HTMLMediaElement",
        members: &[
            ("videoWidth", ReadOnly),
            ("videoHeight", ReadOnly),
            ("poster", Attr("poster")),
            ("playsInline", Flag("playsinline")),
        ],
    },
    Interface {
        name: "HTMLAudioElement",
        parent: "HTMLMediaElement",
        members: &[],
    },
    Interface {
        name: "HTMLLabelElement",
        parent: "HTMLElement",
        members: &[("htmlFor", Attr("for"))],
    },
    Interface {
        name: "HTMLScriptElement",
        parent: "HTMLElement",
        members: &[("src", Attr("src")), ("type", Attr("type"))],
    },
    Interface {
        name: "HTMLLinkElement",
        parent: "HTMLElement",
        members: &[("href", Attr("href")), ("rel", Attr("rel"))],
    },
    Interface {
        name: "HTMLStyleElement",
        parent: "HTMLElement",
        members: &[("sheet", ReadOnly), ("media", Attr("media"))],
    },
    Interface {
        name: "HTMLDivElement",
        parent: "HTMLElement",
        members: &[],
    },
    Interface {
        name: "HTMLSpanElement",
        parent: "HTMLElement",
        members: &[],
    },
    Interface {
        name: "HTMLParagraphElement",
        parent: "HTMLElement",
        members: &[],
    },
    Interface {
        name: "HTMLHeadingElement",
        parent: "HTMLElement",
        members: &[],
    },
    Interface {
        name: "HTMLUListElement",
        parent: "HTMLElement",
        members: &[],
    },
    Interface {
        name: "HTMLOListElement",
        parent: "HTMLElement",
        members: &[],
    },
    Interface {
        name: "HTMLLIElement",
        parent: "HTMLElement",
        members: &[],
    },
    Interface {
        name: "HTMLBodyElement",
        parent: "HTMLElement",
        members: &[],
    },
    Interface {
        name: "HTMLHeadElement",
        parent: "HTMLElement",
        members: &[],
    },
    Interface {
        name: "HTMLHtmlElement",
        parent: "HTMLElement",
        members: &[],
    },
    Interface {
        name: "HTMLTableElement",
        parent: "HTMLElement",
        members: &[],
    },
    Interface {
        name: "HTMLSlotElement",
        parent: "HTMLElement",
        members: &[
            ("name", Attr("name")),
            ("assignedNodes", Method(0)),
            ("assignedElements", Method(0)),
        ],
    },
    Interface {
        name: "HTMLUnknownElement",
        parent: "HTMLElement",
        members: &[],
    },
];

// the interfaces that add form validation and text selection, besides their own members
const VALIDATING: &[&str] = &[
    "HTMLInputElement",
    "HTMLTextAreaElement",
    "HTMLSelectElement",
    "HTMLButtonElement",
];
const SELECTABLE: &[&str] = &["HTMLInputElement", "HTMLTextAreaElement"];

// constants shared by Node and its prototype
const NODE_CONSTANTS: &[(&str, f64)] = &[
    ("ELEMENT_NODE", 1.0),
    ("ATTRIBUTE_NODE", 2.0),
    ("TEXT_NODE", 3.0),
    ("CDATA_SECTION_NODE", 4.0),
    ("PROCESSING_INSTRUCTION_NODE", 7.0),
    ("COMMENT_NODE", 8.0),
    ("DOCUMENT_NODE", 9.0),
    ("DOCUMENT_TYPE_NODE", 10.0),
    ("DOCUMENT_FRAGMENT_NODE", 11.0),
    ("DOCUMENT_POSITION_DISCONNECTED", 1.0),
    ("DOCUMENT_POSITION_PRECEDING", 2.0),
    ("DOCUMENT_POSITION_FOLLOWING", 4.0),
    ("DOCUMENT_POSITION_CONTAINS", 8.0),
    ("DOCUMENT_POSITION_CONTAINED_BY", 16.0),
    ("DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC", 32.0),
];

pub(super) struct DomInterfaces {
    prototypes: HashMap<&'static str, ObjectHandle>,
}

impl DomInterfaces {
    pub(super) fn prototype(&self, name: &str) -> ObjectHandle {
        self.prototypes
            .get(name)
            .cloned()
            .expect("every interface named by the DOM has a prototype")
    }
}

fn define_member(build: &Builder, prototype: &ObjectHandle, entry: &'static (&'static str, Kind)) {
    let (name, kind) = *entry;
    let member = |access| DomMember { entry, access };
    let (getter, setter) = match kind {
        Method(length) => {
            let function = build.function(Native::Dom(member(Access::Call)), name, length);
            prototype.borrow_mut().define(
                name.to_string(),
                Property {
                    enumerable: true,
                    ..Property::data(Value::Object(function))
                },
            );
            return;
        }
        ReadOnly => (true, false),
        ReadWrite | Attr(_) | Flag(_) | Handler => (true, true),
    };
    let get = getter.then(|| {
        Value::Object(build.function(Native::Dom(member(Access::Get)), &format!("get {name}"), 0))
    });
    let set = setter.then(|| {
        Value::Object(build.function(Native::Dom(member(Access::Set)), &format!("set {name}"), 1))
    });
    prototype.borrow_mut().define(
        name.to_string(),
        Property {
            slot: Slot::Accessor { get, set },
            enumerable: true,
            writable: false,
            configurable: true,
        },
    );
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> DomInterfaces {
    let mut prototypes: HashMap<&'static str, ObjectHandle> = HashMap::new();
    for interface in INTERFACES {
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
            define_member(build, &prototype, entry);
        }
        if VALIDATING.contains(&interface.name) {
            for entry in FORM_VALIDATION {
                define_member(build, &prototype, entry);
            }
        }
        if SELECTABLE.contains(&interface.name) {
            for entry in TEXT_SELECTION {
                define_member(build, &prototype, entry);
            }
        }
        if HANDLING.contains(&interface.name) {
            for entry in EVENT_HANDLERS {
                define_member(build, &prototype, entry);
            }
        }
        prototype.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property {
                writable: false,
                ..Property::hidden(Value::String(interface.name.into()))
            },
        );
        let constructor = build.constructor(
            Native::DomConstructor(interface.name),
            interface.name,
            0,
            &prototype,
        );
        if interface.name == "Node" {
            for &(name, value) in NODE_CONSTANTS {
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
    // the legacy factories share their element's prototype rather than owning one
    for (name, element) in [("Image", "HTMLImageElement"), ("Audio", "HTMLAudioElement")] {
        let factory = build.function(Native::DomConstructor(name), name, 0);
        factory.borrow_mut().define(
            "prototype".to_string(),
            Property {
                slot: Slot::Data(Value::Object(prototypes[element].clone())),
                enumerable: false,
                writable: false,
                configurable: false,
            },
        );
        globals.push((name, Value::Object(factory)));
    }
    let image_data = build.function(Native::DomConstructor("ImageData"), "ImageData", 2);
    globals.push(("ImageData", Value::Object(image_data)));
    // signals come from a controller or from these statics, never from new
    let abort_signal = build.function(Native::DomConstructor("AbortSignal"), "AbortSignal", 0);
    for (name, builtin) in [
        ("abort", Builtin::AbortSignalAbort),
        ("timeout", Builtin::AbortSignalTimeout),
        ("any", Builtin::AbortSignalAny),
        ("@@hasInstance", Builtin::AbortSignalHasInstance),
    ] {
        abort_signal
            .borrow_mut()
            .define(name.to_string(), Property::hidden(Value::Builtin(builtin)));
    }
    globals.push(("AbortSignal", Value::Object(abort_signal)));
    DomInterfaces { prototypes }
}

// the most specific interface for a node
pub(super) fn interface_of(document: &Document, node: NodeId) -> &'static str {
    match document.kind(node) {
        Some(NodeKind::Document) => "Document",
        Some(NodeKind::Text(_)) => "Text",
        Some(NodeKind::Comment(_)) => "Comment",
        Some(NodeKind::Fragment) if document.shadow_host(node).is_some() => "ShadowRoot",
        Some(NodeKind::Fragment) => "DocumentFragment",
        Some(NodeKind::Element(element)) => match element.namespace {
            Namespace::Svg => match element.name.as_str() {
                "svg" => "SVGSVGElement",
                "g" | "path" | "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon"
                | "text" | "tspan" | "use" | "image" | "foreignObject" | "a" | "switch" => {
                    "SVGGraphicsElement"
                }
                _ => "SVGElement",
            },
            Namespace::MathMl => "Element",
            Namespace::Html => match element.name.as_str() {
                "input" => "HTMLInputElement",
                "textarea" => "HTMLTextAreaElement",
                "select" => "HTMLSelectElement",
                "option" => "HTMLOptionElement",
                "button" => "HTMLButtonElement",
                "form" => "HTMLFormElement",
                "canvas" => "HTMLCanvasElement",
                "dialog" => "HTMLDialogElement",
                "template" => "HTMLTemplateElement",
                "a" => "HTMLAnchorElement",
                "img" => "HTMLImageElement",
                "video" => "HTMLVideoElement",
                "audio" => "HTMLAudioElement",
                "label" => "HTMLLabelElement",
                "script" => "HTMLScriptElement",
                "link" => "HTMLLinkElement",
                "style" => "HTMLStyleElement",
                "div" => "HTMLDivElement",
                "span" => "HTMLSpanElement",
                "p" => "HTMLParagraphElement",
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "HTMLHeadingElement",
                "ul" => "HTMLUListElement",
                "ol" => "HTMLOListElement",
                "li" => "HTMLLIElement",
                "body" => "HTMLBodyElement",
                "head" => "HTMLHeadElement",
                "html" => "HTMLHtmlElement",
                "table" => "HTMLTableElement",
                "slot" => "HTMLSlotElement",
                name if is_known_html_element(name) || name.contains('-') => "HTMLElement",
                _ => "HTMLUnknownElement",
            },
        },
        None => "Node",
    }
}

fn is_known_html_element(name: &str) -> bool {
    matches!(
        name,
        "abbr"
            | "address"
            | "article"
            | "aside"
            | "b"
            | "bdi"
            | "bdo"
            | "cite"
            | "code"
            | "dd"
            | "dfn"
            | "dt"
            | "em"
            | "figcaption"
            | "figure"
            | "footer"
            | "header"
            | "hgroup"
            | "i"
            | "kbd"
            | "main"
            | "mark"
            | "nav"
            | "noscript"
            | "rp"
            | "rt"
            | "ruby"
            | "s"
            | "samp"
            | "section"
            | "small"
            | "strong"
            | "sub"
            | "summary"
            | "sup"
            | "u"
            | "var"
            | "wbr"
            | "br"
            | "hr"
            | "pre"
            | "blockquote"
            | "dl"
            | "fieldset"
            | "legend"
            | "details"
            | "dialog"
            | "menu"
            | "meta"
            | "title"
            | "base"
            | "iframe"
            | "video"
            | "audio"
            | "source"
            | "track"
            | "picture"
            | "svg"
            | "slot"
            | "output"
            | "progress"
            | "meter"
            | "optgroup"
            | "datalist"
            | "caption"
            | "colgroup"
            | "col"
            | "tbody"
            | "thead"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "data"
            | "time"
            | "q"
            | "del"
            | "ins"
            | "map"
            | "area"
            | "object"
            | "embed"
            | "param"
    )
}

impl ScriptRuntime {
    // the wrapper object of a node or event value, which holds its expandos and prototype
    pub(super) fn wrapper_object(&self, value: &Value) -> Option<ObjectHandle> {
        match value {
            Value::Element(handle) => Some(handle.object.clone()),
            Value::Event(event) => Some(event.object.clone()),
            Value::Range(range) => Some(range.object.clone()),
            Value::Selection(selection) => Some(selection.object.clone()),
            Value::Promise(promise) => Some(super::promise_object(promise)),
            Value::Document => Some(self.node_handle(self.document_node()).object.clone()),
            _ => None,
        }
    }

    pub(super) fn new_node_wrapper(&self, node: NodeId) -> ObjectHandle {
        let interface = interface_of(&self.document.borrow(), node);
        new_object(JsObject::with_prototype(
            Some(self.intrinsics.dom.prototype(interface)),
            ObjectClass::Ordinary,
        ))
    }

    // the node a DOM method runs on, with the document standing in as its root node
    fn this_node(&mut self, this: &Value, member: DomMember) -> Result<ElementHandle, ScriptError> {
        match this {
            Value::Element(handle) => Ok(handle.clone()),
            Value::Document => Ok(self.node_handle(self.document_node())),
            _ => Err(self.type_error(format!(
                "Illegal invocation of '{}' on a value that is not a node",
                member.name()
            ))),
        }
    }

    // a DOM member with its custom element reactions, which run before the call returns
    pub(super) fn call_dom(
        &mut self,
        member: DomMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        if member.name() == "createElement"
            && matches!(this, Value::Document)
            && !self.custom_definitions.is_empty()
        {
            let name = match arguments.first() {
                Some(name) => self.coerce_string(name.clone())?.to_ascii_lowercase(),
                None => String::new(),
            };
            if let Some(element) = self.create_custom_element(&name)? {
                return Ok(element);
            }
        }
        let result = self.call_dom_member(member, this, arguments);
        if !self.custom_definitions.is_empty() {
            self.run_element_reactions()?;
        }
        result
    }

    fn call_dom_member(
        &mut self,
        member: DomMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        if let Some(result) = self.call_event_target(member, &this, &arguments)? {
            return Ok(result);
        }
        let handle = self.this_node(&this, member)?;
        let node = handle.id;
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        match (member.reflect(), member.access) {
            (Reflect::Text(attribute), Access::Get) => {
                return Ok(Value::String(
                    self.document
                        .borrow()
                        .attribute(node, attribute)
                        .unwrap_or_default()
                        .into(),
                ));
            }
            (Reflect::Flag(attribute), Access::Get) => {
                return Ok(Value::Bool(
                    self.document.borrow().attribute(node, attribute).is_some(),
                ));
            }
            (Reflect::Text(attribute), Access::Set) => {
                let value = self.coerce_string(first)?;
                self.set_element_attribute(&handle, attribute, value);
                return Ok(Value::Undefined);
            }
            (Reflect::Flag(attribute), Access::Set) => {
                if first.truthy() {
                    self.set_element_attribute(&handle, attribute, String::new());
                } else {
                    self.remove_element_attribute(&handle, attribute);
                }
                return Ok(Value::Undefined);
            }
            _ => {}
        }
        if let Some(result) = self.call_dom_native(member, &this, &handle, &arguments)? {
            return Ok(result);
        }
        // the remaining members are implemented on the host values themselves
        let parsed_document = !matches!(this, Value::Document)
            && matches!(
                self.document.borrow().kind(handle.id),
                Some(NodeKind::Document)
            );
        let target = match (&this, member.name()) {
            (
                Value::Document,
                "getElementById" | "querySelector" | "querySelectorAll" | "createElement"
                | "createTextNode" | "createRange" | "getSelection" | "documentElement"
                | "activeElement" | "importNode" | "adoptNode" | "createTreeWalker"
                | "createNodeIterator" | "getAnimations",
            ) => Value::Document,
            // a parsed document makes its nodes in the same tree store as the page's
            (
                _,
                "createElement" | "createTextNode" | "createRange" | "getSelection"
                | "activeElement" | "importNode" | "adoptNode" | "createTreeWalker"
                | "createNodeIterator" | "getAnimations",
            ) if parsed_document => Value::Document,
            _ => Value::Element(handle),
        };
        match member.access {
            Access::Get => self.member(target, member.name()),
            Access::Set => {
                self.assign_member(target, member.name(), first)?;
                Ok(Value::Undefined)
            }
            Access::Call => {
                let method = self.member(target.clone(), member.name())?;
                self.call_function(method, target, arguments)
            }
        }
    }

    // EventTarget members and on* handlers, which the window shares with nodes
    fn call_event_target(
        &mut self,
        member: DomMember,
        this: &Value,
        arguments: &[Value],
    ) -> Result<Option<Value>, ScriptError> {
        let name = member.name();
        let handler = matches!(member.entry.1, Handler);
        if !handler
            && !matches!(
                name,
                "addEventListener" | "removeEventListener" | "dispatchEvent"
            )
        {
            return Ok(None);
        }
        // called bare, as addEventListener(...) in a script, the target is the window
        let this = match this {
            Value::Undefined | Value::Null => &Value::Window,
            this => this,
        };
        let Some(target) = self.listener_target(this) else {
            return Err(self.type_error(format!(
                "Illegal invocation of '{name}' on a value that is not an EventTarget"
            )));
        };
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        Ok(Some(match (name, member.access) {
            (_, Access::Get) => self.event_handler(target, &name[2..]),
            (_, Access::Set) => {
                self.set_event_handler(target, &name[2..], first);
                Value::Undefined
            }
            ("addEventListener", _) => {
                self.add_event_listener(target, arguments)?;
                Value::Undefined
            }
            ("removeEventListener", _) => {
                self.remove_event_listener(target, arguments)?;
                Value::Undefined
            }
            _ => self.dispatch_event_from_script(target, &first)?,
        }))
    }

    fn selector_list(&mut self, source: &str) -> Result<Vec<crate::css::Selector>, ScriptError> {
        crate::css::Selector::parse_list(source).map_err(|_| {
            self.throw_value(dom_exception(
                "SyntaxError",
                &format!("'{source}' is not a valid selector."),
            ))
        })
    }

    pub(super) fn node_list(&self, nodes: Vec<NodeId>) -> Value {
        Value::NodeList(Rc::new(
            nodes
                .into_iter()
                .map(|node| self.node_handle(node))
                .collect(),
        ))
    }

    fn optional_node(&self, node: Option<NodeId>) -> Value {
        node.map_or(Value::Null, |node| self.node_value(node))
    }

    // members implemented here rather than on the host values; None hands the call on
    fn call_dom_native(
        &mut self,
        member: DomMember,
        this: &Value,
        handle: &ElementHandle,
        arguments: &[Value],
    ) -> Result<Option<Value>, ScriptError> {
        let node = handle.id;
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        let second = arguments.get(1).cloned().unwrap_or(Value::Undefined);
        // the page's document, or one DOMParser made
        let on_document = matches!(this, Value::Document)
            || matches!(self.document.borrow().kind(node), Some(NodeKind::Document));
        let parsed_document = on_document && !matches!(this, Value::Document);
        if member.access != Access::Set
            && let Some(measured) = self.element_geometry(node, member.name())?
        {
            return Ok(Some(measured));
        }
        if let Some(value) = self.media_member(node, member.name(), member.access, first.clone())? {
            return Ok(Some(value));
        }
        let value = match (member.name(), member.access) {
            ("tagName", Access::Get) | ("localName", Access::Get) => {
                let document = self.document.borrow();
                document.element(node).map_or(Value::Null, |element| {
                    Value::String(
                        if member.name() == "tagName" {
                            element.tag_name()
                        } else {
                            element.name.clone()
                        }
                        .into(),
                    )
                })
            }
            ("namespaceURI", Access::Get) => self
                .document
                .borrow()
                .element(node)
                .map_or(Value::Null, |element| {
                    Value::String(element.namespace.uri().into())
                }),
            ("ownerDocument", Access::Get) => {
                if on_document {
                    Value::Null
                } else {
                    let top = {
                        let document = self.document.borrow();
                        let mut top = node;
                        while let Some(parent) = document.parent(top) {
                            top = parent;
                        }
                        matches!(document.kind(top), Some(NodeKind::Document)).then_some(top)
                    };
                    match top {
                        Some(top) if top != self.document_node() => self.node_value(top),
                        _ => Value::Document,
                    }
                }
            }
            ("sheet", Access::Get) => self.style_sheet_of(node),
            ("fonts", Access::Get) if on_document => self.font_set(),
            ("requestFullscreen", Access::Call) => self.request_fullscreen(node)?,
            ("exitFullscreen", Access::Call) if on_document => {
                if self.leave_fullscreen()? {
                    Value::Promise(self.fulfilled_promise(Value::Undefined))
                } else {
                    Value::Promise(self.rejected_promise("Document not active"))
                }
            }
            ("fullscreenElement", Access::Get) if on_document => {
                let element = self.fullscreen_element;
                self.optional_node(element)
            }
            ("fullscreenEnabled", Access::Get) if on_document => Value::Bool(true),
            ("fullscreen", Access::Get) if on_document => {
                Value::Bool(self.fullscreen_element.is_some())
            }
            ("styleSheets", Access::Get) => self.style_sheets_of(node),
            ("adoptedStyleSheets", Access::Get) => self.adopted_sheets_value(node),
            ("adoptedStyleSheets", Access::Set) => {
                self.set_adopted_sheets(node, first)?;
                Value::Undefined
            }
            ("documentElement", Access::Get) if parsed_document => {
                let element = self.document.borrow().document_element_of(node);
                self.optional_node(element)
            }
            ("getElementById", Access::Call) if parsed_document => {
                let id = self.coerce_string(first)?;
                let found = {
                    let document = self.document.borrow();
                    document.descendants(node).find(|&candidate| {
                        document
                            .element(candidate)
                            .is_some_and(|element| element.attr("id") == Some(id.as_str()))
                    })
                };
                self.optional_node(found)
            }
            ("textContent", Access::Get) if on_document => Value::Null,
            ("nodeValue", Access::Get) if self.document.borrow().character_data(node).is_none() => {
                Value::Null
            }
            ("length", Access::Get) => Value::Number(super::strings::utf16_length(
                self.document
                    .borrow()
                    .character_data(node)
                    .unwrap_or_default(),
            ) as f64),
            ("wholeText", Access::Get) => {
                let document = self.document.borrow();
                let siblings = document
                    .parent(node)
                    .map(|parent| document.children(parent).to_vec())
                    .unwrap_or_else(|| vec![node]);
                let index = siblings
                    .iter()
                    .position(|&sibling| sibling == node)
                    .unwrap_or(0);
                let start = siblings[..index]
                    .iter()
                    .rposition(|&sibling| !document.is_text(sibling))
                    .map_or(0, |position| position + 1);
                let end = siblings[index..]
                    .iter()
                    .position(|&sibling| !document.is_text(sibling))
                    .map_or(siblings.len(), |position| index + position);
                Value::String(
                    siblings[start..end]
                        .iter()
                        .filter_map(|&sibling| document.text(sibling))
                        .collect::<String>()
                        .into(),
                )
            }
            ("innerText", Access::Get) => {
                Value::String(self.document.borrow().text_content(node).into())
            }
            ("innerText", Access::Set) => {
                let text = self.coerce_string(first)?;
                self.set_element_text_content(handle, text)?;
                Value::Undefined
            }
            ("attributes", Access::Get) => {
                let attributes = self
                    .document
                    .borrow()
                    .element(node)
                    .map(|element| element.attrs.clone())
                    .unwrap_or_default();
                let entries = attributes
                    .into_iter()
                    .map(|(name, value)| {
                        self.object_from_entries(vec![
                            ("name".to_string(), Value::String(name.clone().into())),
                            ("localName".to_string(), Value::String(name.into())),
                            ("value".to_string(), Value::String(value.into())),
                            ("namespaceURI".to_string(), Value::Null),
                            ("specified".to_string(), Value::Bool(true)),
                        ])
                    })
                    .collect();
                Self::new_array(entries)
            }
            ("content", Access::Get) => {
                let content = self.document.borrow_mut().ensure_template_content(node);
                self.optional_node(content)
            }
            ("body", Access::Get) => {
                let body = self.document.borrow().body_of(node);
                self.optional_node(body)
            }
            ("head", Access::Get) => {
                let head = self.document.borrow().head_of(node);
                self.optional_node(head)
            }
            ("title", Access::Get) if on_document => Value::String(
                self.document
                    .borrow()
                    .title_of(node)
                    .unwrap_or_default()
                    .into(),
            ),
            ("title", Access::Set) if on_document => {
                let title = self.coerce_string(first)?;
                let existing = {
                    let document = self.document.borrow();
                    document.descendants(node).find(|&candidate| {
                        document
                            .element(candidate)
                            .is_some_and(|element| element.name == "title")
                    })
                };
                let element = match existing {
                    Some(element) => element,
                    None => {
                        let created = self.document.borrow_mut().create_element("title");
                        let head = self.document.borrow().head_of(node);
                        let parent = head
                            .or_else(|| self.document.borrow().document_element_of(node))
                            .unwrap_or(node);
                        self.insert_node(parent, created, None)?;
                        created
                    }
                };
                let element = self.node_handle(element);
                self.set_element_text_content(&element, title)?;
                Value::Undefined
            }
            ("defaultView", Access::Get) => Value::Window,
            ("readyState", Access::Get) => Value::String("complete".into()),
            ("visibilityState", Access::Get) => {
                Value::String(if self.visible { "visible" } else { "hidden" }.into())
            }
            ("hidden", Access::Get) => Value::Bool(!self.visible),
            ("width" | "height", Access::Get) => {
                self.canvas_element_member(handle, member.name(), None)?
            }
            ("width" | "height", Access::Set) => {
                self.canvas_element_member(handle, member.name(), Some(first))?
            }
            ("setPointerCapture" | "releasePointerCapture" | "hasPointerCapture", Access::Call) => {
                self.pointer_capture_member(node, member.name(), first)?
            }
            ("returnValue", _)
            | ("show" | "showModal" | "close" | "requestClose", Access::Call) => {
                self.dialog_member(handle, member.name(), member.access, first)?
            }
            ("toDataURL" | "toBlob", Access::Call) => {
                self.export_canvas(node, member.name(), arguments)?
            }
            ("naturalWidth" | "naturalHeight" | "complete" | "currentSrc", Access::Get)
            | ("decode", Access::Call) => self.image_member(node, member.name()),
            ("crossOrigin", Access::Get) => {
                match self.document.borrow().attribute(node, "crossorigin") {
                    None => Value::Null,
                    Some(mode) if mode.eq_ignore_ascii_case("use-credentials") => {
                        Value::String("use-credentials".into())
                    }
                    Some(_) => Value::String("anonymous".into()),
                }
            }
            ("crossOrigin", Access::Set) => {
                if matches!(first, Value::Null) {
                    self.remove_element_attribute(handle, "crossorigin");
                } else {
                    let mode = self.coerce_string(first)?;
                    self.set_element_attribute(handle, "crossorigin", mode);
                }
                Value::Undefined
            }
            ("decoding" | "loading", Access::Get) => {
                let (keywords, default): (&[&str], &str) = match member.name() {
                    "decoding" => (&["sync", "async", "auto"], "auto"),
                    _ => (&["lazy", "eager"], "eager"),
                };
                let document = self.document.borrow();
                let keyword = document
                    .attribute(node, member.name())
                    .and_then(|value| {
                        keywords
                            .iter()
                            .find(|keyword| keyword.eq_ignore_ascii_case(value))
                    })
                    .copied()
                    .unwrap_or(default);
                Value::String(keyword.into())
            }
            ("decoding" | "loading", Access::Set) => {
                let keyword = self.coerce_string(first)?;
                self.set_element_attribute(handle, member.name(), keyword);
                Value::Undefined
            }
            ("hasAttribute", Access::Call) => {
                let name = self.coerce_string(first)?;
                Value::Bool(self.document.borrow().attribute(node, &name).is_some())
            }
            ("hasAttributes", Access::Call) => Value::Bool(
                self.document
                    .borrow()
                    .element(node)
                    .is_some_and(|element| !element.attrs.is_empty()),
            ),
            ("getAttributeNames", Access::Call) => {
                let names = self
                    .document
                    .borrow()
                    .element(node)
                    .map(|element| element.attrs.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                Self::new_array(
                    names
                        .into_iter()
                        .map(|name| Value::String(name.into()))
                        .collect(),
                )
            }
            ("toggleAttribute", Access::Call) => {
                let name = self.coerce_string(first)?;
                let present = self.document.borrow().attribute(node, &name).is_some();
                let enabled = match second {
                    Value::Undefined => !present,
                    force => force.truthy(),
                };
                if enabled && !present {
                    self.set_element_attribute(handle, &name, String::new());
                } else if !enabled && present {
                    self.remove_element_attribute(handle, &name);
                }
                Value::Bool(enabled)
            }
            ("matches" | "webkitMatchesSelector", Access::Call) => {
                let source = self.coerce_string(first)?;
                let selectors = self.selector_list(&source)?;
                Value::Bool(self.document.borrow().matches_selectors(node, &selectors))
            }
            ("closest", Access::Call) => {
                let source = self.coerce_string(first)?;
                let selectors = self.selector_list(&source)?;
                let found = {
                    let document = self.document.borrow();
                    std::iter::once(node)
                        .chain(document.ancestors(node))
                        .find(|&candidate| document.matches_selectors(candidate, &selectors))
                };
                self.optional_node(found)
            }
            ("getElementsByTagName", Access::Call) => {
                let name = self.coerce_string(first)?.to_ascii_lowercase();
                let found = {
                    let document = self.document.borrow();
                    document
                        .descendants(node)
                        .filter(|&candidate| {
                            document
                                .element(candidate)
                                .is_some_and(|element| name == "*" || element.name == name)
                        })
                        .collect()
                };
                self.node_list(found)
            }
            ("getElementsByClassName", Access::Call) => {
                let names = self.coerce_string(first)?;
                let classes = names.split_ascii_whitespace().collect::<Vec<_>>();
                let found = {
                    let document = self.document.borrow();
                    document
                        .descendants(node)
                        .filter(|&candidate| {
                            !classes.is_empty()
                                && document.element(candidate).is_some_and(|element| {
                                    classes.iter().all(|class| element.has_class(class))
                                })
                        })
                        .collect()
                };
                self.node_list(found)
            }
            ("getElementById", Access::Call) if !on_document => {
                let id = self.coerce_string(first)?;
                let found = {
                    let document = self.document.borrow();
                    document
                        .descendants(node)
                        .find(|&candidate| document.attribute(candidate, "id") == Some(id.as_str()))
                };
                self.optional_node(found)
            }
            ("contains", Access::Call) => match self.node_object_id(&first) {
                Some(other) => {
                    Value::Bool(self.document.borrow().is_inclusive_ancestor(node, other))
                }
                None => Value::Bool(false),
            },
            ("isSameNode", Access::Call) => Value::Bool(self.node_object_id(&first) == Some(node)),
            ("lookupNamespaceURI", Access::Call) => {
                let prefix = match first {
                    Value::Undefined | Value::Null => None,
                    value => Some(self.coerce_string(value)?).filter(|prefix| !prefix.is_empty()),
                };
                self.lookup_namespace(node, prefix.as_deref())
            }
            ("evaluate", Access::Call) if on_document => self.call_xpath_evaluator(
                super::xpath::XPathMember::Evaluate,
                arguments,
                "Document",
            )?,
            ("createExpression", Access::Call) if on_document => self.call_xpath_evaluator(
                super::xpath::XPathMember::CreateExpression,
                arguments,
                "Document",
            )?,
            ("createNSResolver", Access::Call) if on_document => first,
            ("isEqualNode", Access::Call) => match self.node_object_id(&first) {
                Some(other) => Value::Bool(equal_nodes(&self.document.borrow(), node, other)),
                None => Value::Bool(false),
            },
            ("getRootNode", Access::Call) => {
                let composed = if super::operations::is_primitive(&first) {
                    false
                } else {
                    self.get_property(&first, "composed")?.truthy()
                };
                let document = self.document.borrow();
                let root = if composed {
                    document.shadow_including_root(node)
                } else {
                    document.tree_root(node)
                };
                drop(document);
                self.node_value(root)
            }
            ("compareDocumentPosition", Access::Call) => {
                let Some(other) = self.node_object_id(&first) else {
                    return Err(self.type_error(
                        "Failed to execute 'compareDocumentPosition' on 'Node': parameter 1 is not of type 'Node'.",
                    ));
                };
                Value::Number(f64::from(document_position(
                    &self.document.borrow(),
                    node,
                    other,
                )))
            }
            ("replaceChild", Access::Call) => {
                let child = self.node_argument(arguments, 0, "replaceChild")?;
                let old = self.node_argument(arguments, 1, "replaceChild")?;
                if self.document.borrow().parent(old) != Some(node) {
                    return Err(self.throw_value(dom_exception(
                        "NotFoundError",
                        "The node to be replaced is not a child of this node.",
                    )));
                }
                if child != old {
                    let checked = self
                        .document
                        .borrow()
                        .validate_insertion(node, child, Some(old));
                    if let Err(error) = checked {
                        return Err(self.dom_error(error));
                    }
                    // a replacement that already follows the old node goes where it stood,
                    // ahead of what came after it
                    let next = {
                        let document = self.document.borrow();
                        document
                            .sibling(old, true)
                            .and_then(|next| match next == child {
                                true => document.sibling(child, true),
                                false => Some(next),
                            })
                    };
                    self.remove_node(old)?;
                    let reference = next;
                    self.insert_node(node, child, reference)?;
                }
                self.node_value(old)
            }
            ("insertAdjacentElement", Access::Call) | ("insertAdjacentText", Access::Call) => {
                let position = self.coerce_string(first)?.to_ascii_lowercase();
                let inserted = if member.name() == "insertAdjacentText" {
                    let text = self.coerce_string(second)?;
                    self.document.borrow_mut().create_text(text)
                } else {
                    self.node_argument(arguments, 1, "insertAdjacentElement")?
                };
                let (parent, reference) = {
                    let document = self.document.borrow();
                    match position.as_str() {
                        "beforebegin" => (document.parent(node), Some(node)),
                        "afterbegin" => (Some(node), document.children(node).first().copied()),
                        "beforeend" => (Some(node), None),
                        "afterend" => (document.parent(node), document.sibling(node, true)),
                        _ => {
                            drop(document);
                            return Err(self.throw_value(dom_exception(
                                "SyntaxError",
                                &format!("The value provided ('{position}') is not one of 'beforeBegin', 'afterBegin', 'beforeEnd', or 'afterEnd'."),
                            )));
                        }
                    }
                };
                match parent {
                    Some(parent) => {
                        self.insert_node(parent, inserted, reference)?;
                        if member.name() == "insertAdjacentText" {
                            Value::Undefined
                        } else {
                            self.node_value(inserted)
                        }
                    }
                    None => Value::Null,
                }
            }
            ("createElementNS", Access::Call) => {
                let namespace = match first {
                    Value::Null | Value::Undefined => None,
                    uri => Namespace::from_uri(&self.coerce_string(uri)?),
                };
                let qualified = self.coerce_string(second)?;
                let local = qualified
                    .split_once(':')
                    .map_or(qualified.as_str(), |(_, local)| local)
                    .to_string();
                let created = match namespace {
                    Some(namespace) => self
                        .document
                        .borrow_mut()
                        .create_element_ns(namespace, &local),
                    None => self.document.borrow_mut().create_element(&local),
                };
                self.node_value(created)
            }
            ("createComment", Access::Call) => {
                let data = self.coerce_string(first)?;
                let created = self.document.borrow_mut().create_comment(data);
                self.node_value(created)
            }
            ("elementsFromPoint", Access::Call) if on_document => {
                let x = self.coerce_number(first)?;
                let y = self.coerce_number(second)?;
                let elements = self.elements_from_point(x, y);
                Self::new_array(
                    elements
                        .into_iter()
                        .map(|node| self.node_value(node))
                        .collect(),
                )
            }
            ("checkVisibility", Access::Call) => Value::Bool(self.check_visibility(node, &first)?),
            (name, access) if name == "role" || name.starts_with("aria") => {
                let attribute = match name {
                    "role" => "role".to_string(),
                    _ => format!("aria-{}", name[4..].to_ascii_lowercase()),
                };
                match access {
                    Access::Get => self
                        .document
                        .borrow()
                        .attribute(node, &attribute)
                        .map_or(Value::Null, |value| Value::String(value.into())),
                    _ => {
                        if matches!(first, Value::Null) {
                            self.remove_element_attribute(handle, &attribute);
                        } else {
                            let value = self.coerce_string(first)?;
                            self.set_element_attribute(handle, &attribute, value);
                        }
                        Value::Undefined
                    }
                }
            }
            ("getAttributeNS" | "hasAttributeNS" | "removeAttributeNS", Access::Call) => {
                let local = self.coerce_string(second)?;
                let found = self.document.borrow().element(node).and_then(|element| {
                    element
                        .attrs
                        .keys()
                        .find(|name| {
                            **name == local
                                || name.rsplit_once(':').is_some_and(|(_, tail)| tail == local)
                        })
                        .cloned()
                });
                match member.name() {
                    "getAttributeNS" => found
                        .and_then(|name| {
                            self.document
                                .borrow()
                                .attribute(node, &name)
                                .map(String::from)
                        })
                        .map_or(Value::Null, |value| Value::String(value.into())),
                    "hasAttributeNS" => Value::Bool(found.is_some()),
                    _ => {
                        if let Some(name) = found {
                            self.remove_element_attribute(handle, &name);
                        }
                        Value::Undefined
                    }
                }
            }
            ("setAttributeNS", Access::Call) => {
                let name = self.coerce_string(second)?;
                let value =
                    self.coerce_string(arguments.get(2).cloned().unwrap_or(Value::Undefined))?;
                self.set_element_attribute(handle, &name, value);
                Value::Undefined
            }
            ("getHTML", Access::Call) => {
                self.member(Value::Element(handle.clone()), "innerHTML")?
            }
            ("baseURI", Access::Get) | ("URL" | "documentURI", Access::Get) => {
                Value::String(self.location.borrow().to_string().into())
            }
            ("characterSet" | "charset" | "inputEncoding", Access::Get) => {
                Value::String("UTF-8".into())
            }
            ("contentType", Access::Get) => Value::String("text/html".into()),
            ("compatMode", Access::Get) => Value::String("CSS1Compat".into()),
            ("referrer", Access::Get) => Value::String("".into()),
            ("location", Access::Get) if on_document => self.member(Value::Window, "location")?,
            ("scrollingElement", Access::Get) => {
                let root = self.document.borrow().document_element_of(node);
                self.optional_node(root)
            }
            ("dir", _) if on_document => {
                let root = self.document.borrow().document_element_of(node);
                match (root, member.access) {
                    (Some(root), Access::Set) => {
                        let value = self.coerce_string(first)?;
                        let root = self.node_handle(root);
                        self.set_element_attribute(&root, "dir", value);
                        Value::Undefined
                    }
                    (Some(root), _) => Value::String(
                        self.document
                            .borrow()
                            .attribute(root, "dir")
                            .map(str::to_ascii_lowercase)
                            .filter(|dir| matches!(dir.as_str(), "ltr" | "rtl" | "auto"))
                            .unwrap_or_default()
                            .into(),
                    ),
                    _ => Value::String("".into()),
                }
            }
            ("cookie", Access::Get) => {
                let now = self.now_milliseconds();
                self.cookies
                    .retain(|cookie| cookie.2.is_none_or(|expires| expires > now));
                Value::String(
                    self.cookies
                        .iter()
                        .map(|(name, value, _)| {
                            if name.is_empty() {
                                value.clone()
                            } else {
                                format!("{name}={value}")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("; ")
                        .into(),
                )
            }
            ("cookie", Access::Set) => {
                let text = self.coerce_string(first)?;
                self.set_cookie(&text);
                Value::Undefined
            }
            (
                "forms" | "images" | "links" | "scripts" | "embeds" | "plugins" | "anchors",
                Access::Get,
            ) if on_document => {
                let found = {
                    let document = self.document.borrow();
                    document
                        .descendants(node)
                        .filter(|&candidate| {
                            document
                                .element(candidate)
                                .is_some_and(|element| match member.name() {
                                    "forms" => element.name == "form",
                                    "images" => element.name == "img",
                                    "scripts" => element.name == "script",
                                    "embeds" | "plugins" => element.name == "embed",
                                    "anchors" => {
                                        element.name == "a" && element.attr("name").is_some()
                                    }
                                    _ => {
                                        matches!(element.name.as_str(), "a" | "area")
                                            && element.attr("href").is_some()
                                    }
                                })
                        })
                        .collect()
                };
                self.node_list(found)
            }
            ("getElementsByName", Access::Call) => {
                let name = self.coerce_string(first)?;
                let found = {
                    let document = self.document.borrow();
                    document
                        .descendants(node)
                        .filter(|&candidate| {
                            document.attribute(candidate, "name") == Some(name.as_str())
                        })
                        .collect()
                };
                self.node_list(found)
            }
            ("hasFocus", Access::Call) => Value::Bool(self.visible),
            ("tabIndex", Access::Get) => Value::Number(f64::from(self.tab_index(node))),
            ("tabIndex", Access::Set) => {
                let index = super::operations::to_int32_number(self.coerce_number(first)?);
                self.set_element_attribute(handle, "tabindex", index.to_string());
                Value::Undefined
            }
            ("draggable" | "spellcheck" | "translate", Access::Set) => {
                let on = first.truthy();
                let value = match (member.name(), on) {
                    ("translate", true) => "yes",
                    ("translate", false) => "no",
                    (_, true) => "true",
                    (_, false) => "false",
                };
                self.set_element_attribute(handle, member.name(), value.to_string());
                Value::Undefined
            }
            ("draggable", Access::Get) => {
                let document = self.document.borrow();
                Value::Bool(match document.attribute(node, "draggable") {
                    Some(value) if value.eq_ignore_ascii_case("true") => true,
                    Some(value) if value.eq_ignore_ascii_case("false") => false,
                    _ => document.element(node).is_some_and(|element| {
                        element.name == "img"
                            || (matches!(element.name.as_str(), "a" | "area")
                                && element.attr("href").is_some())
                    }),
                })
            }
            ("spellcheck" | "translate", Access::Get) => {
                let (on, off) = match member.name() {
                    "spellcheck" => (["", "true"], "false"),
                    _ => (["", "yes"], "no"),
                };
                let document = self.document.borrow();
                let inherited = std::iter::once(node)
                    .chain(document.ancestors(node))
                    .find_map(|ancestor| {
                        let value = document.attribute(ancestor, member.name())?;
                        if on.iter().any(|keyword| value.eq_ignore_ascii_case(keyword)) {
                            Some(true)
                        } else if value.eq_ignore_ascii_case(off) {
                            Some(false)
                        } else {
                            None
                        }
                    });
                Value::Bool(inherited.unwrap_or(true))
            }
            ("contentEditable", Access::Get) => Value::String(
                content_editable_state(self.document.borrow().attribute(node, "contenteditable"))
                    .unwrap_or("inherit")
                    .into(),
            ),
            ("contentEditable", Access::Set) => {
                let value = self.coerce_string(first)?.to_ascii_lowercase();
                match value.as_str() {
                    "inherit" => {
                        self.remove_element_attribute(handle, "contenteditable");
                    }
                    "true" | "false" | "plaintext-only" => {
                        self.set_element_attribute(handle, "contenteditable", value);
                    }
                    _ => {
                        return Err(self.throw_value(dom_exception(
                            "SyntaxError",
                            "The value provided is not one of 'true', 'false', 'plaintext-only', or 'inherit'.",
                        )));
                    }
                }
                Value::Undefined
            }
            ("isContentEditable", Access::Get) => {
                let document = self.document.borrow();
                let state = std::iter::once(node)
                    .chain(document.ancestors(node))
                    .find_map(|ancestor| {
                        content_editable_state(document.attribute(ancestor, "contenteditable"))
                    });
                Value::Bool(state.is_some_and(|state| state != "false"))
            }
            ("scrollIntoView", Access::Call) => {
                self.scroll_into_view(node, &first)?;
                Value::Undefined
            }
            ("scroll" | "scrollTo" | "scrollBy", Access::Call) => {
                let offset = Self::scroll_target(
                    self.scroll_offset(node),
                    arguments,
                    member.name() == "scrollBy",
                )?;
                if self.set_scroll_offset(node, offset) {
                    self.dispatch_node_event(node, "scroll")?;
                }
                Value::Undefined
            }
            ("elementFromPoint", Access::Call) if on_document => {
                let x = self.coerce_number(first)?;
                let y = self.coerce_number(second)?;
                self.element_from_point(x, y)
                    .map_or(Value::Null, |node| self.node_value(node))
            }
            ("createDocumentFragment", Access::Call) => {
                let created = self.document.borrow_mut().create_fragment();
                self.node_value(created)
            }
            ("attachShadow", Access::Call) => {
                let mode = match first.resolve() {
                    options if !super::operations::is_primitive(&options) => {
                        let mode = self.get_property(&options, "mode")?;
                        self.coerce_string(mode)?
                    }
                    _ => String::new(),
                };
                if mode != "open" && mode != "closed" {
                    return Err(self.type_error(
                        "Failed to execute 'attachShadow' on 'Element': The provided value is not a valid enum value of type ShadowRootMode.",
                    ));
                }
                let allowed = self.document.borrow().element(node).is_some_and(|element| {
                    element.name.contains('-')
                        || matches!(
                            element.name.as_str(),
                            "article"
                                | "aside"
                                | "blockquote"
                                | "body"
                                | "div"
                                | "footer"
                                | "h1"
                                | "h2"
                                | "h3"
                                | "h4"
                                | "h5"
                                | "h6"
                                | "header"
                                | "main"
                                | "nav"
                                | "p"
                                | "section"
                                | "span"
                        )
                });
                let root = allowed
                    .then(|| {
                        self.document
                            .borrow_mut()
                            .attach_shadow(node, mode == "open")
                    })
                    .flatten();
                match root {
                    Some(root) => self.node_value(root),
                    None => {
                        return Err(self.throw_value(dom_exception(
                            "NotSupportedError",
                            "Failed to execute 'attachShadow' on 'Element': This element does not support attachShadow",
                        )));
                    }
                }
            }
            ("shadowRoot", Access::Get) => {
                let document = self.document.borrow();
                let open = document
                    .shadow_root(node)
                    .filter(|&root| document.shadow_host(root).is_some_and(|(_, open)| open));
                drop(document);
                self.optional_node(open)
            }
            ("assignedSlot", Access::Get) => {
                let slot = self.document.borrow().assigned_slot(node);
                self.optional_node(slot)
            }
            ("assignedNodes", Access::Call) | ("assignedElements", Access::Call) => {
                let document = self.document.borrow();
                let nodes = document
                    .assigned_nodes(node)
                    .into_iter()
                    .filter(|&assigned| {
                        member.name() == "assignedNodes" || document.element(assigned).is_some()
                    })
                    .collect::<Vec<_>>();
                drop(document);
                Self::new_array(
                    nodes
                        .into_iter()
                        .map(|node| self.node_value(node))
                        .collect(),
                )
            }
            ("host", Access::Get) => {
                let host = self
                    .document
                    .borrow()
                    .shadow_host(node)
                    .map(|(host, _)| host);
                self.optional_node(host)
            }
            ("mode", Access::Get) => {
                let open = self
                    .document
                    .borrow()
                    .shadow_host(node)
                    .map(|(_, open)| open);
                Value::String(
                    if open == Some(false) {
                        "closed"
                    } else {
                        "open"
                    }
                    .into(),
                )
            }
            ("innerHTML", _) if self.document.borrow().shadow_host(node).is_some() => {
                if member.access == Access::Get {
                    Value::String(self.document.borrow().inner_html(node).into())
                } else {
                    let source = self.coerce_string(first)?;
                    let fragment = self.parse_fragment(&source)?;
                    self.replace_children_quietly(node, Some(fragment));
                    Value::Undefined
                }
            }
            // innerHTML that also turns <template shadowrootmode> into shadow roots
            ("setHTMLUnsafe", Access::Call) => {
                let source = self.coerce_string(first)?;
                let fragment = self.parse_fragment(&source)?;
                self.document
                    .borrow_mut()
                    .attach_declarative_shadows(fragment);
                let target = self
                    .document
                    .borrow_mut()
                    .ensure_template_content(node)
                    .unwrap_or(node);
                self.replace_children_quietly(target, Some(fragment));
                Value::Undefined
            }
            ("startViewTransition", Access::Call) => self.start_view_transition(first)?,
            ("getBBox", Access::Call) => self.svg_bounding_box(node),
            ("execCommand", Access::Call) => {
                let command = self.coerce_string(first)?;
                let value = match arguments.get(2).cloned() {
                    None | Some(Value::Undefined) => String::new(),
                    Some(value) => self.coerce_string(value)?,
                };
                Value::Bool(self.exec_command(&command, &value)?)
            }
            ("queryCommandSupported", Access::Call) | ("queryCommandEnabled", Access::Call) => {
                let command = self.coerce_string(first)?.to_ascii_lowercase();
                let known = matches!(
                    command.as_str(),
                    "copy" | "cut" | "selectall" | "inserttext" | "delete" | "forwarddelete"
                );
                let enabled = match command.as_str() {
                    "copy" | "cut" => self.transiently_active(),
                    _ => self
                        .focused_element
                        .is_some_and(|node| self.is_text_control(node)),
                };
                Value::Bool(known && (member.name() == "queryCommandSupported" || enabled))
            }
            ("queryCommandState", Access::Call) => Value::Bool(false),
            ("queryCommandValue", Access::Call) => Value::String("".into()),
            ("activeViewTransition", Access::Get) => self.active_view_transition(),
            ("currentScript", Access::Get) if on_document => self.current_script_value(),
            ("attributeStyleMap", Access::Get) => self.style_property_map(node, false),
            ("computedStyleMap", Access::Call) => self.style_property_map(node, true),
            ("createEvent", Access::Call) => {
                let interface = self.coerce_string(first)?;
                self.create_legacy_event(&interface)?
            }
            _ => return Ok(None),
        };
        Ok(Some(value))
    }

    // the node behind a value, for methods that compare or take nodes
    fn node_object_id(&self, value: &Value) -> Option<NodeId> {
        match value.resolve() {
            Value::Element(handle) => Some(handle.id),
            Value::Document => Some(self.document_node()),
            _ => None,
        }
    }

    pub(super) fn construct_dom(
        &mut self,
        interface: &'static str,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        if interface.starts_with("HTML") && matches!(new_target, Value::Object(_)) {
            let own = self.get_property(&Value::Window, interface)?;
            if !values_equal(&own, &new_target) {
                return self.construct_custom_element(&new_target);
            }
        }
        if interface == "Image" {
            return self.construct_image(&arguments);
        }
        if interface == "Audio" {
            return self.construct_audio_element(&arguments);
        }
        if interface == "ImageData" {
            return self.construct_image_data(&arguments);
        }
        if interface == "AbortSignal" {
            return Err(self.type_error("Illegal constructor"));
        }
        if interface == "EventTarget" {
            let prototype = match new_target {
                Value::Object(_) => self.prototype_from_constructor(&new_target)?,
                _ => self.intrinsics.dom.prototype("EventTarget"),
            };
            return Ok(Value::Object(self.new_event_target(prototype)));
        }
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        let data = |runtime: &mut Self| match first.clone() {
            Value::Undefined => Ok(String::new()),
            value => runtime.coerce_string(value),
        };
        let node = match interface {
            "Text" => {
                let text = data(self)?;
                self.document.borrow_mut().create_text(text)
            }
            "Comment" => {
                let text = data(self)?;
                self.document.borrow_mut().create_comment(text)
            }
            "DocumentFragment" => self.document.borrow_mut().create_fragment(),
            _ => return Err(self.type_error("Illegal constructor")),
        };
        Ok(self.node_value(node))
    }
}

fn equal_nodes(document: &Document, left: NodeId, right: NodeId) -> bool {
    document.kind(left) == document.kind(right)
        && document.children(left).len() == document.children(right).len()
        && document
            .children(left)
            .iter()
            .zip(document.children(right))
            .all(|(&left, &right)| equal_nodes(document, left, right))
}

// compareDocumentPosition's bit mask
fn document_position(document: &Document, node: NodeId, other: NodeId) -> u16 {
    const DISCONNECTED: u16 = 1;
    const PRECEDING: u16 = 2;
    const FOLLOWING: u16 = 4;
    const CONTAINS: u16 = 8;
    const CONTAINED_BY: u16 = 16;
    const IMPLEMENTATION_SPECIFIC: u16 = 32;
    if node == other {
        return 0;
    }
    if document.tree_root(node) != document.tree_root(other) {
        let order = if other < node { PRECEDING } else { FOLLOWING };
        return DISCONNECTED | IMPLEMENTATION_SPECIFIC | order;
    }
    if document.is_inclusive_ancestor(other, node) {
        return CONTAINS | PRECEDING;
    }
    if document.is_inclusive_ancestor(node, other) {
        return CONTAINED_BY | FOLLOWING;
    }
    match document.tree_order(other, node) {
        TreeOrdering::Less => PRECEDING,
        _ => FOLLOWING,
    }
}

// the keyword a contenteditable attribute value stands for, none when it is missing or invalid
fn content_editable_state(value: Option<&str>) -> Option<&'static str> {
    match value?.to_ascii_lowercase().as_str() {
        "" | "true" => Some("true"),
        "false" => Some("false"),
        "plaintext-only" => Some("plaintext-only"),
        _ => None,
    }
}
