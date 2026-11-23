// FileReader: reads a Blob or File as text, a data URL, an ArrayBuffer or a binary string,
// and reports the outcome from a task as loadstart, progress, load and loadend

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::realm::{Builder, Native};
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReaderMember {
    ReadAsText,
    ReadAsDataUrl,
    ReadAsArrayBuffer,
    ReadAsBinaryString,
    Abort,
    Result,
    Error,
    ReadyState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Text,
    DataUrl,
    ArrayBuffer,
    BinaryString,
}

const EMPTY: u8 = 0;
const LOADING: u8 = 1;
const DONE: u8 = 2;

pub(super) struct ReaderInterfaces {
    prototype: ObjectHandle,
}

pub(super) fn create(
    build: &Builder,
    event_target: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> ReaderInterfaces {
    let prototype = new_object(JsObject::with_prototype(
        Some(event_target.clone()),
        ObjectClass::Ordinary,
    ));
    for (name, member, length) in [
        ("readAsText", ReaderMember::ReadAsText, 1),
        ("readAsDataURL", ReaderMember::ReadAsDataUrl, 1),
        ("readAsArrayBuffer", ReaderMember::ReadAsArrayBuffer, 1),
        ("readAsBinaryString", ReaderMember::ReadAsBinaryString, 1),
        ("abort", ReaderMember::Abort, 0),
    ] {
        let function = build.function(Native::Reader(member), name, length);
        prototype.borrow_mut().define(
            name.to_string(),
            Property {
                enumerable: true,
                ..Property::data(Value::Object(function))
            },
        );
    }
    for (name, member) in [
        ("result", ReaderMember::Result),
        ("error", ReaderMember::Error),
        ("readyState", ReaderMember::ReadyState),
    ] {
        let get = build.function(Native::Reader(member), &format!("get {name}"), 0);
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
        Property::hidden(Value::String("FileReader".into())),
    );
    let constructor = build.constructor(Native::ReaderConstructor, "FileReader", 0, &prototype);
    for (name, value) in [("EMPTY", EMPTY), ("LOADING", LOADING), ("DONE", DONE)] {
        for target in [&constructor, &prototype] {
            target.borrow_mut().define(
                name.to_string(),
                Property {
                    enumerable: true,
                    writable: false,
                    configurable: false,
                    ..Property::data(Value::Number(f64::from(value)))
                },
            );
        }
    }
    globals.push(("FileReader", Value::Object(constructor)));
    ReaderInterfaces { prototype }
}

#[derive(Default)]
struct ReaderState {
    ready: u8,
    result: Option<Value>,
    error: Option<Value>,
    // bumped by every read and abort, so a task of an earlier read does nothing
    generation: u64,
    pending: Option<(Format, Vec<u8>, String, Option<String>)>,
}

#[derive(Default)]
pub(super) struct Readers {
    states: HashMap<u32, ReaderState>,
}

// the text of bytes under an encoding label: a byte order mark wins, then the label, then utf-8
fn decode(bytes: &[u8], label: Option<&str>) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    let utf16 = |rest: &[u8], big: bool| {
        let units: Vec<u16> = rest
            .chunks(2)
            .map(|pair| {
                let pair = [pair[0], pair.get(1).copied().unwrap_or(0)];
                if big {
                    u16::from_be_bytes(pair)
                } else {
                    u16::from_le_bytes(pair)
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return utf16(rest, false);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return utf16(rest, true);
    }
    match label
        .map(|label| label.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("utf-16le" | "utf-16") => utf16(bytes, false),
        Some("utf-16be") => utf16(bytes, true),
        Some("iso-8859-1" | "latin1" | "windows-1252" | "ascii" | "us-ascii") => {
            bytes.iter().map(|&byte| char::from(byte)).collect()
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

impl ScriptRuntime {
    pub(super) fn construct_reader(&mut self, new_target: Value) -> Result<Value, ScriptError> {
        let prototype = match new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.readers.prototype.clone(),
        };
        let object = self.new_event_target(prototype);
        let ObjectClass::EventTarget(id) = object.borrow().class else {
            unreachable!("new_event_target makes event targets")
        };
        self.readers.states.insert(id, ReaderState::default());
        Ok(Value::Object(object))
    }

    pub(super) fn call_reader(
        &mut self,
        member: ReaderMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let id = match &this {
            Value::Object(object) => match object.borrow().class {
                ObjectClass::EventTarget(id) if self.readers.states.contains_key(&id) => Some(id),
                _ => None,
            },
            _ => None,
        };
        let Some(id) = id else {
            return Err(self.type_error("Illegal invocation"));
        };
        let format = match member {
            ReaderMember::Result => {
                let state = &self.readers.states[&id];
                return Ok(state.result.clone().unwrap_or(Value::Null));
            }
            ReaderMember::Error => {
                let state = &self.readers.states[&id];
                return Ok(state.error.clone().unwrap_or(Value::Null));
            }
            ReaderMember::ReadyState => {
                return Ok(Value::Number(f64::from(self.readers.states[&id].ready)));
            }
            ReaderMember::Abort => {
                let state = self.readers.states.get_mut(&id).expect("checked above");
                if state.ready != LOADING {
                    return Ok(Value::Undefined);
                }
                state.ready = DONE;
                state.result = None;
                state.pending = None;
                state.generation += 1;
                let _held = self.held_targets.remove(&id);
                for name in ["abort", "loadend"] {
                    self.fire(
                        events::ListenerTarget::Object(id),
                        name,
                        events::HostEventFields::default(),
                    )?;
                }
                return Ok(Value::Undefined);
            }
            ReaderMember::ReadAsText => Format::Text,
            ReaderMember::ReadAsDataUrl => Format::DataUrl,
            ReaderMember::ReadAsArrayBuffer => Format::ArrayBuffer,
            ReaderMember::ReadAsBinaryString => Format::BinaryString,
        };
        let (bytes, mime_type) = match arguments.first().map(Value::resolve) {
            Some(Value::Blob(blob)) => (blob.bytes.clone(), blob.mime_type.clone()),
            Some(Value::File(file)) => (file.bytes.clone(), file.mime_type.clone()),
            _ => {
                return Err(self.type_error(
                    "Failed to execute 'read' on 'FileReader': parameter 1 is not of type 'Blob'.",
                ));
            }
        };
        if self.readers.states[&id].ready == LOADING {
            return Err(self.throw_value(dom_exception(
                "InvalidStateError",
                "Failed to execute 'read' on 'FileReader': The object is already busy reading Blobs.",
            )));
        }
        let label = match (format, arguments.get(1)) {
            (Format::Text, Some(label)) if !matches!(label, Value::Undefined) => {
                Some(self.coerce_string(label.clone())?.to_string())
            }
            _ => None,
        };
        let state = self.readers.states.get_mut(&id).expect("checked above");
        state.ready = LOADING;
        state.result = None;
        state.error = None;
        state.generation += 1;
        state.pending = Some((format, bytes, mime_type, label));
        let generation = state.generation;
        // a reader at work stays alive for its listeners
        if let Some(object) = self.event_targets.get(&id).and_then(Weak::upgrade) {
            self.held_targets.insert(id, object);
        }
        self.schedule_native_task(Native::ReaderDeliver(id, generation));
        Ok(Value::Undefined)
    }

    pub(super) fn deliver_reader(
        &mut self,
        id: u32,
        generation: u64,
    ) -> Result<Value, ScriptError> {
        let Some(state) = self.readers.states.get_mut(&id) else {
            return Ok(Value::Undefined);
        };
        if state.generation != generation {
            return Ok(Value::Undefined);
        }
        let Some((format, bytes, mime_type, label)) = state.pending.take() else {
            return Ok(Value::Undefined);
        };
        let target = events::ListenerTarget::Object(id);
        let plain = events::HostEventFields::default;
        self.fire(target, "loadstart", plain())?;
        if self
            .readers
            .states
            .get(&id)
            .is_none_or(|state| state.generation != generation)
        {
            return Ok(Value::Undefined);
        }
        self.fire(target, "progress", plain())?;
        let result = match format {
            Format::Text => Value::String(decode(&bytes, label.as_deref()).into()),
            Format::BinaryString => Value::String(
                bytes
                    .iter()
                    .map(|&byte| char::from(byte))
                    .collect::<String>()
                    .into(),
            ),
            Format::DataUrl => {
                let mime_type = if mime_type.is_empty() {
                    "application/octet-stream"
                } else {
                    &mime_type
                };
                Value::String(format!("data:{mime_type};base64,{}", canvas::base64(&bytes)).into())
            }
            Format::ArrayBuffer => Value::Bytes(Rc::new(RefCell::new(bytes.into()))),
        };
        if let Some(state) = self.readers.states.get_mut(&id)
            && state.generation == generation
        {
            state.ready = DONE;
            state.result = Some(result);
        } else {
            return Ok(Value::Undefined);
        }
        let _held = self.held_targets.remove(&id);
        self.fire(target, "load", plain())?;
        self.fire(target, "loadend", plain())?;
        self.drain_microtasks()?;
        Ok(Value::Undefined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_reads_a_blob_each_way_and_reports_from_a_task() {
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
                const blob = new Blob(["hé"], { type: "text/plain" });
                const text = new FileReader();
                text.onload = () => {
                    seen.push(text.result, text.readyState);
                    const url = new FileReader();
                    url.addEventListener("loadend", () => {
                        seen.push(url.result);
                        const buffer = new FileReader();
                        buffer.onload = () => {
                            seen.push(new Uint8Array(buffer.result).join(","));
                            const binary = new FileReader();
                            binary.onload = () => {
                                seen.push([...binary.result].map((c) => c.charCodeAt(0)).join(","));
                                const stopped = new FileReader();
                                stopped.onload = () => seen.push("loaded after abort");
                                stopped.onabort = () => seen.push("aborted", stopped.result, stopped.readyState);
                                stopped.readAsText(blob);
                                stopped.abort();
                                show();
                            };
                            binary.readAsBinaryString(blob);
                        };
                        buffer.readAsArrayBuffer(blob);
                    });
                    url.readAsDataURL(new Blob([new Uint8Array([1, 2, 3])]));
                };
                text.onloadstart = () => seen.push("start", text.readyState);
                text.readAsText(blob);
                seen.push(text.readyState, text.result, FileReader.DONE, String(text));
                try { text.readAsText(blob); } catch (error) { seen.push(error.name); }
                try { text.readAsText("x"); } catch (error) { seen.push(error.name); }
                show();
                "##,
            )
            .expect("script should run");
        for _ in 0..6 {
            runtime.advance_time(1).expect("reader tasks should run");
        }
        let status = document.borrow().find_element_by_id("status").unwrap();
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some(
                "1||2|[object FileReader]|InvalidStateError|TypeError|start|1|h\u{e9}|2|data:application/octet-stream;base64,AQID|104,195,169|104,195,169|aborted||2"
            )
        );
        assert_eq!(decode(&[0xff, 0xfe, 0x41, 0x00], None), "A");
        assert_eq!(decode(&[0xe9], Some("latin1")), "\u{e9}");
    }
}
