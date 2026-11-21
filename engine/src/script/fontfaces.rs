// FontFace and document.fonts: a face from bytes or from urls of the app, loaded when asked,
// and the set that hands its loaded faces to the page's fonts

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::operations::is_primitive;
use super::realm::{Builder, Native};
use super::*;
use std::cell::Cell;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FontMember {
    FaceConstructor,
    SetConstructor,
    Descriptor(usize),
    SetDescriptor(usize),
    Status,
    Loaded,
    Load,
    Add,
    Delete,
    Clear,
    Has,
    ForEach,
    Values,
    Size,
    Ready,
    SetStatus,
    SetLoad,
    Check,
}

const DESCRIPTORS: [&str; 7] = [
    "family",
    "style",
    "weight",
    "stretch",
    "unicodeRange",
    "display",
    "featureSettings",
];
const DEFAULTS: [&str; 7] = [
    "",
    "normal",
    "normal",
    "normal",
    "U+0-10FFFF",
    "auto",
    "normal",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Unloaded,
    Loading,
    Loaded,
    Error,
}

enum Source {
    Bytes(Vec<u8>),
    // a css src value: urls in order of preference
    Urls(String),
}

pub(super) struct FaceState {
    descriptors: RefCell<[String; 7]>,
    source: Source,
    status: Cell<Status>,
    loaded: PromiseHandle,
    face: RefCell<Option<Rc<crate::font::FontFace>>>,
}

pub(super) struct FontInterfaces {
    face: ObjectHandle,
    set: ObjectHandle,
}

fn native(member: FontMember) -> Native {
    Native::Font(member)
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    event_target: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> FontInterfaces {
    use FontMember as M;
    let accessor = |target: &ObjectHandle, name: &str, get: M, set: Option<M>| {
        let get = build.function(native(get), &format!("get {name}"), 0);
        let set =
            set.map(|set| Value::Object(build.function(native(set), &format!("set {name}"), 1)));
        target.borrow_mut().define(
            name.to_string(),
            Property {
                slot: Slot::Accessor {
                    get: Some(Value::Object(get)),
                    set,
                },
                enumerable: true,
                writable: false,
                configurable: true,
            },
        );
    };
    let method = |target: &ObjectHandle, name: &str, member: M, length: u32| {
        let function = build.function(native(member), name, length);
        target
            .borrow_mut()
            .define(name.to_string(), Property::hidden(Value::Object(function)));
    };
    let tag = |target: &ObjectHandle, name: &str| {
        target.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property {
                writable: false,
                ..Property::hidden(Value::String(name.into()))
            },
        );
    };
    let face = new_object(JsObject::with_prototype(
        Some(object_prototype.clone()),
        ObjectClass::Ordinary,
    ));
    for (index, name) in DESCRIPTORS.iter().enumerate() {
        accessor(
            &face,
            name,
            M::Descriptor(index),
            Some(M::SetDescriptor(index)),
        );
    }
    accessor(&face, "status", M::Status, None);
    accessor(&face, "loaded", M::Loaded, None);
    method(&face, "load", M::Load, 0);
    tag(&face, "FontFace");
    let constructor = build.constructor(native(M::FaceConstructor), "FontFace", 2, &face);
    globals.push(("FontFace", Value::Object(constructor)));
    let set = new_object(JsObject::with_prototype(
        Some(event_target.clone()),
        ObjectClass::Ordinary,
    ));
    for (name, member, length) in [
        ("add", M::Add, 1),
        ("delete", M::Delete, 1),
        ("clear", M::Clear, 0),
        ("has", M::Has, 1),
        ("forEach", M::ForEach, 1),
        ("load", M::SetLoad, 1),
        ("check", M::Check, 1),
    ] {
        method(&set, name, member, length);
    }
    let values = build.function(native(M::Values), "values", 0);
    for key in ["values", "keys", "entries", "@@iterator"] {
        set.borrow_mut().define(
            key.to_string(),
            Property::hidden(Value::Object(values.clone())),
        );
    }
    accessor(&set, "size", M::Size, None);
    accessor(&set, "ready", M::Ready, None);
    accessor(&set, "status", M::SetStatus, None);
    tag(&set, "FontFaceSet");
    let set_constructor = build.constructor(native(M::SetConstructor), "FontFaceSet", 1, &set);
    globals.push(("FontFaceSet", Value::Object(set_constructor)));
    FontInterfaces { face, set }
}

// the weights a font-weight descriptor names, none for the face's own
fn weights(descriptor: &str) -> Option<(u16, u16)> {
    let weight = |word: &str| match word {
        "normal" => Some(400),
        "bold" => Some(700),
        number => number
            .parse::<f32>()
            .ok()
            .map(|value| value.clamp(1.0, 1000.0) as u16),
    };
    let words: Vec<&str> = descriptor.split_whitespace().collect();
    match words.as_slice() {
        [single] => weight(single).map(|value| (value, value)),
        [low, high] => Some((weight(low)?, weight(high)?)),
        _ => None,
    }
}

// the urls of a css src value, in order
fn source_urls(source: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find("url(") {
        let after = &rest[start + 4..];
        let Some(end) = after.find(')') else {
            break;
        };
        let url = after[..end].trim().trim_matches(['"', '\'']).to_string();
        if !url.is_empty() {
            urls.push(url);
        }
        rest = &after[end + 1..];
    }
    urls
}

// the family a CSS font shorthand ends with, as document.fonts.load and check take it
fn shorthand_family(font: &str) -> String {
    let after_size = font
        .split_whitespace()
        .skip_while(|word| {
            !word
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_digit())
        })
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ");
    let family = if after_size.is_empty() {
        font
    } else {
        &after_size
    };
    family
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_matches(['"', '\''])
        .to_ascii_lowercase()
}

impl ScriptRuntime {
    fn face_state(value: &Value) -> Option<Rc<FaceState>> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::FontFace(state) => Some(state.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    // the one FontFaceSet of the document
    pub(super) fn font_set(&mut self) -> Value {
        if let Some(set) = self.font_set.borrow().clone() {
            return Value::Object(set);
        }
        let prototype = self.intrinsics.font_faces.set.clone();
        let set = self.new_event_target(prototype);
        if let ObjectClass::EventTarget(id) = set.borrow().class {
            self.held_targets.insert(id, set.clone());
        }
        *self.font_set.borrow_mut() = Some(set.clone());
        Value::Object(set)
    }

    // hands the loaded faces in document.fonts to the page's fonts
    fn publish_fonts(&mut self) {
        let fonts = self
            .font_set_faces
            .borrow()
            .iter()
            .filter_map(|face| Self::face_state(&Value::Object(face.clone())))
            .filter_map(|state| {
                let face = state.face.borrow().clone()?;
                let descriptors = state.descriptors.borrow();
                Some(crate::dom::ScriptFont {
                    family: descriptors[0].clone(),
                    weights: weights(&descriptors[2]),
                    italic: matches!(descriptors[1].as_str(), "italic" | "oblique")
                        || descriptors[1].starts_with("oblique"),
                    face,
                })
            })
            .collect();
        self.document.borrow_mut().set_script_fonts(fonts);
    }

    fn load_face(&mut self, state: &Rc<FaceState>) {
        if state.status.get() != Status::Unloaded {
            return;
        }
        state.status.set(Status::Loading);
        let parsed = match &state.source {
            Source::Bytes(bytes) => crate::font::FontFace::parse(bytes.clone()).ok(),
            Source::Urls(source) => {
                let resources = self.resources.clone().unwrap_or_default();
                let provider = self.resource_provider.clone();
                let base = self.base_url.clone();
                source_urls(source).into_iter().find_map(|reference| {
                    let url = base.as_ref()?.resolve(&reference).ok()?;
                    let resource =
                        resolve_runtime_resource(&resources, provider.as_deref(), &url).ok()??;
                    crate::font::FontFace::parse(resource.body().to_vec()).ok()
                })
            }
        };
        match parsed {
            Some(face) => {
                *state.face.borrow_mut() = Some(Rc::new(face));
                state.status.set(Status::Loaded);
                self.settle_promise(&state.loaded, PromiseOutcome::Fulfilled(Value::Undefined));
            }
            None => {
                state.status.set(Status::Error);
                state.loaded.borrow_mut().handled = true;
                let error = dom_exception("NetworkError", "A network error occurred.");
                self.settle_promise(&state.loaded, PromiseOutcome::Rejected(error));
            }
        }
    }

    pub(super) fn construct_font_face(
        &mut self,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let mut arguments = arguments.into_iter();
        let family = arguments.next().unwrap_or(Value::Undefined);
        let family = self.coerce_string(family)?;
        let source = arguments.next().unwrap_or(Value::Undefined);
        let source = match source {
            Value::String(text) => Source::Urls(text.to_string()),
            Value::TypedArray(_) | Value::Bytes(_) => {
                Source::Bytes(self.buffer_source_bytes(&source)?)
            }
            other => Source::Urls(self.coerce_string(other)?),
        };
        let mut descriptors = DEFAULTS.map(str::to_string);
        descriptors[0] = family;
        if let Some(given) = arguments.next().filter(|given| !is_primitive(given)) {
            for (index, name) in DESCRIPTORS.iter().enumerate().skip(1) {
                let value = self.get_property(&given, name)?;
                if !matches!(value, Value::Undefined) {
                    descriptors[index] = self.coerce_string(value)?;
                }
            }
        }
        let bytes = matches!(source, Source::Bytes(_));
        let state = Rc::new(FaceState {
            descriptors: RefCell::new(descriptors),
            source,
            status: Cell::new(Status::Unloaded),
            loaded: Self::pending_promise(),
            face: RefCell::new(None),
        });
        // a face given its bytes loads at once
        if bytes {
            self.load_face(&state);
        }
        let prototype = match &new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.font_faces.face.clone(),
        };
        Ok(Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::FontFace(state),
        ))))
    }

    pub(super) fn call_font(
        &mut self,
        member: FontMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use FontMember as M;
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        match member {
            M::FaceConstructor | M::SetConstructor => {
                return Err(self.type_error("Illegal constructor"));
            }
            M::Descriptor(_) | M::SetDescriptor(_) | M::Status | M::Loaded | M::Load => {
                let Some(state) = Self::face_state(&this) else {
                    return Err(self.type_error("Illegal invocation"));
                };
                return Ok(match member {
                    M::Descriptor(index) => {
                        Value::String(state.descriptors.borrow()[index].clone().into())
                    }
                    M::SetDescriptor(index) => {
                        state.descriptors.borrow_mut()[index] = self.coerce_string(first)?;
                        if self.font_set_faces.borrow().iter().any(|face| matches!(&this, Value::Object(object) if Rc::ptr_eq(object, face))) {
                            self.publish_fonts();
                        }
                        Value::Undefined
                    }
                    M::Status => Value::String(
                        match state.status.get() {
                            Status::Unloaded => "unloaded",
                            Status::Loading => "loading",
                            Status::Loaded => "loaded",
                            Status::Error => "error",
                        }
                        .into(),
                    ),
                    M::Loaded => Value::Promise(state.loaded.clone()),
                    _ => {
                        self.load_face(&state);
                        if self.font_set_faces.borrow().iter().any(|face| matches!(&this, Value::Object(object) if Rc::ptr_eq(object, face))) {
                            self.publish_fonts();
                        }
                        let face = this.clone();
                        let promise = state.loaded.clone();
                        let outcome = promise.borrow().outcome.clone();
                        match outcome {
                            PromiseOutcome::Fulfilled(_) => {
                                Value::Promise(self.fulfilled_promise(face))
                            }
                            _ => Value::Promise(promise),
                        }
                    }
                });
            }
            _ => {}
        }
        let faces = self.font_set_faces.borrow().clone();
        let position = |faces: &[ObjectHandle], value: &Value| {
            faces
                .iter()
                .position(|face| matches!(value, Value::Object(object) if Rc::ptr_eq(object, face)))
        };
        match member {
            M::Add => {
                let Value::Object(face) = &first else {
                    return Err(self.type_error("Failed to execute 'add' on 'FontFaceSet': parameter 1 is not of type 'FontFace'."));
                };
                let Some(state) = Self::face_state(&first) else {
                    return Err(self.type_error("Failed to execute 'add' on 'FontFaceSet': parameter 1 is not of type 'FontFace'."));
                };
                if position(&faces, &first).is_none() {
                    self.font_set_faces.borrow_mut().push(face.clone());
                    self.load_face(&state);
                    self.publish_fonts();
                }
                Ok(this)
            }
            M::Delete => {
                let Some(index) = position(&faces, &first) else {
                    return Ok(Value::Bool(false));
                };
                self.font_set_faces.borrow_mut().remove(index);
                self.publish_fonts();
                Ok(Value::Bool(true))
            }
            M::Clear => {
                self.font_set_faces.borrow_mut().clear();
                self.publish_fonts();
                Ok(Value::Undefined)
            }
            M::Has => Ok(Value::Bool(position(&faces, &first).is_some())),
            M::Size => Ok(Value::Number(faces.len() as f64)),
            M::Values => {
                let list = Self::new_array(faces.into_iter().map(Value::Object).collect());
                let values = self.get_property(&list, "values")?;
                self.call_function(values, list, Vec::new())
            }
            M::ForEach => {
                let receiver = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                for face in faces {
                    let face = Value::Object(face);
                    self.call_function(
                        first.clone(),
                        receiver.clone(),
                        vec![face.clone(), face, this.clone()],
                    )?;
                }
                Ok(Value::Undefined)
            }
            M::Ready => Ok(Value::Promise(self.fulfilled_promise(this))),
            M::SetStatus => Ok(Value::String("loaded".into())),
            M::SetLoad | M::Check => {
                let font = self.coerce_string(first)?;
                let family = shorthand_family(&font);
                let matching: Vec<ObjectHandle> = faces
                    .into_iter()
                    .filter(|face| {
                        Self::face_state(&Value::Object(face.clone())).is_some_and(|state| {
                            state.descriptors.borrow()[0].to_ascii_lowercase() == family
                        })
                    })
                    .collect();
                if member == M::Check {
                    return Ok(Value::Bool(matching.iter().all(|face| {
                        Self::face_state(&Value::Object(face.clone()))
                            .is_some_and(|state| state.status.get() == Status::Loaded)
                    })));
                }
                for face in &matching {
                    if let Some(state) = Self::face_state(&Value::Object(face.clone())) {
                        self.load_face(&state);
                    }
                }
                self.publish_fonts();
                let loaded = matching
                    .into_iter()
                    .filter(|face| {
                        Self::face_state(&Value::Object(face.clone()))
                            .is_some_and(|state| state.status.get() == Status::Loaded)
                    })
                    .map(Value::Object)
                    .collect();
                Ok(Value::Promise(
                    self.fulfilled_promise(Self::new_array(loaded)),
                ))
            }
            _ => Err(self.type_error("Illegal invocation")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_and_shorthands_read_as_css_writes_them() {
        assert_eq!(weights("bold"), Some((700, 700)));
        assert_eq!(weights("100 900"), Some((100, 900)));
        assert_eq!(weights("normal"), Some((400, 400)));
        assert_eq!(
            source_urls("url(\"/a.woff2\") format(\"woff2\"), url(b.ttf)"),
            ["/a.woff2", "b.ttf"]
        );
        assert_eq!(
            shorthand_family("bold 16px \"Inter Var\", sans-serif"),
            "inter var"
        );
        assert_eq!(shorthand_family("12px serif"), "serif");
    }
}
