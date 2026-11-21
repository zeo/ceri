// copy, cut and paste events and the data they carry between a page and the host clipboard

use super::*;

// what a clipboard event's clipboardData holds: the text formats by type, read from the
// clipboard for a paste and written by the handlers of a copy or cut
pub(super) struct Transfer {
    data: Vec<(String, String)>,
    // only a copy or a cut lets its handlers change the data
    writable: bool,
}

pub(crate) type TransferHandle = Rc<RefCell<Transfer>>;

// "text" is the old name of text/plain, and types compare without case
fn format(value: &Value) -> String {
    match value.string_value().trim().to_ascii_lowercase().as_str() {
        "text" => "text/plain".to_string(),
        other => other.to_string(),
    }
}

impl ScriptRuntime {
    // fires the copy, cut or paste that a shortcut stands for at the element the keys go to;
    // true when a handler took the default over, which for a copy or cut puts what it set on
    // the clipboard
    pub(super) fn clipboard_shortcut(
        &mut self,
        target: NodeId,
        shortcut: &str,
    ) -> Result<bool, ScriptError> {
        let name = match shortcut {
            "c" => "copy",
            "x" => "cut",
            _ => "paste",
        };
        let provider = self.clipboard_provider.clone();
        let mut data = Vec::new();
        let mut files = Vec::new();
        if name == "paste"
            && let Some(provider) = &provider
        {
            // a clipboard that cannot be read pastes nothing
            if let Ok(text) = provider.read_text()
                && !text.is_empty()
            {
                data.push(("text/plain".to_string(), text));
            }
            if let Ok(Some((mime_type, bytes))) = provider.read_image() {
                let extension = mime_type.rsplit('/').next().unwrap_or("png");
                files.push(Rc::new(FileData {
                    bytes,
                    name: format!("image.{extension}"),
                    mime_type,
                    last_modified: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |elapsed| elapsed.as_millis() as i64),
                }));
            }
        }
        let transfer = Rc::new(RefCell::new(Transfer {
            data,
            writable: name != "paste",
        }));
        let fields = events::HostEventFields {
            value: Some(transfer_object(&transfer, &files)),
            ..events::HostEventFields::default()
        };
        let outcome = self.fire(events::ListenerTarget::Node(target), name, fields)?;
        self.drain_microtasks()?;
        if !outcome.default_prevented {
            return Ok(false);
        }
        let set = transfer
            .borrow()
            .data
            .iter()
            .find(|(kind, _)| kind == "text/plain")
            .map(|(_, text)| text.clone());
        if name != "paste"
            && let (Some(provider), Some(text)) = (provider, set)
        {
            provider
                .write_text(&text)
                .map_err(|error| runtime_error(format!("clipboard write failed: {error}")))?;
        }
        Ok(true)
    }

    pub(super) fn call_clipboard_member(
        &mut self,
        builtin: Builtin,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        Ok(match builtin {
            Builtin::ClipboardGetData(transfer) => {
                let wanted = format(&first);
                let transfer = transfer.borrow();
                let held = transfer.data.iter().find(|(kind, _)| *kind == wanted);
                Value::String(held.map_or(String::new(), |(_, text)| text.clone()).into())
            }
            Builtin::ClipboardSetData(transfer) => {
                let mut transfer = transfer.borrow_mut();
                if transfer.writable {
                    let kind = format(&first);
                    let text = arguments.get(1).map_or(String::new(), Value::string_value);
                    transfer.data.retain(|(held, _)| *held != kind);
                    transfer.data.push((kind, text));
                }
                Value::Undefined
            }
            Builtin::ClipboardClearData(transfer) => {
                let mut transfer = transfer.borrow_mut();
                if transfer.writable {
                    match arguments.first() {
                        Some(kind) => {
                            let kind = format(kind);
                            transfer.data.retain(|(held, _)| *held != kind);
                        }
                        None => transfer.data.clear(),
                    }
                }
                Value::Undefined
            }
            Builtin::ClipboardItemString(text) => {
                if let Some(text) = text
                    && is_callable(&first)
                {
                    self.call(first, vec![Value::String(text.to_string().into())])?;
                }
                Value::Undefined
            }
            Builtin::ClipboardItemFile(file) => file.map_or(Value::Null, Value::File),
            _ => Value::Undefined,
        })
    }
}

// the DataTransfer a clipboard event hands its listeners; types, items and files show what
// the transfer held when the event began
fn transfer_object(handle: &TransferHandle, files: &[FileHandle]) -> Value {
    let text = |value: &str| Value::String(value.to_string().into());
    let item = |kind: &str, mime: &str, string: Option<&str>, file: Option<&FileHandle>| {
        plain_object([
            ("kind".to_string(), text(kind)),
            ("type".to_string(), text(mime)),
            (
                "getAsString".to_string(),
                Value::Builtin(Builtin::ClipboardItemString(string.map(Rc::from))),
            ),
            (
                "getAsFile".to_string(),
                Value::Builtin(Builtin::ClipboardItemFile(file.cloned())),
            ),
        ])
    };
    let transfer = handle.borrow();
    let mut types: Vec<Value> = transfer.data.iter().map(|(kind, _)| text(kind)).collect();
    let mut items: Vec<Value> = transfer
        .data
        .iter()
        .map(|(kind, held)| item("string", kind, Some(held), None))
        .collect();
    if !files.is_empty() {
        types.push(text("Files"));
    }
    items.extend(
        files
            .iter()
            .map(|file| item("file", &file.mime_type, None, Some(file))),
    );
    let method = |builtin: Builtin| Value::Builtin(builtin);
    plain_object([
        ("types".to_string(), ScriptRuntime::new_array(types)),
        ("items".to_string(), ScriptRuntime::new_array(items)),
        (
            "files".to_string(),
            ScriptRuntime::new_array(files.iter().cloned().map(Value::File).collect()),
        ),
        ("dropEffect".to_string(), text("none")),
        ("effectAllowed".to_string(), text("uninitialized")),
        (
            "getData".to_string(),
            method(Builtin::ClipboardGetData(handle.clone())),
        ),
        (
            "setData".to_string(),
            method(Builtin::ClipboardSetData(handle.clone())),
        ),
        (
            "clearData".to_string(),
            method(Builtin::ClipboardClearData(handle.clone())),
        ),
    ])
}
