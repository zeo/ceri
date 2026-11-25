// copy, cut and paste events and the data they carry between a page and the host clipboard

use super::*;

// what a clipboard event's clipboardData holds: the text formats by type, read from the
// clipboard for a paste and written by the handlers of a copy or cut
pub(super) struct Transfer {
    data: Vec<(String, String)>,
    files: Vec<FileHandle>,
    // only a copy or a cut lets its handlers change the data
    writable: bool,
}

pub(crate) type TransferHandle = Rc<RefCell<Transfer>>;

// a ClipboardItem's types, each with the blob, text or promise of one it was made with
pub(crate) type ItemHandle = Rc<Vec<(String, Value)>>;

// a clipboard write waiting on promised pieces: each type with its bytes once they arrive,
// how many are still to come, and the promise write handed back
pub(crate) struct Write {
    pieces: Vec<(String, Option<Vec<u8>>)>,
    waiting: usize,
    done: PromiseHandle,
}

pub(crate) type WriteHandle = Rc<RefCell<Write>>;

impl Write {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        tracer.trace_promise(&self.done);
    }
}

fn not_allowed(message: &str) -> Value {
    dom_exception("NotAllowedError", message)
}

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
            files,
            writable: name != "paste",
        }));
        let fields = events::HostEventFields {
            value: Some(self.transfer_object(&transfer)),
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
                if let Some(text) = *text
                    && is_callable(&first)
                {
                    self.call(first, vec![Value::String(text.to_string().into())])?;
                }
                Value::Undefined
            }
            Builtin::ClipboardItemFile(file) => file.map_or(Value::Null, Value::File),
            Builtin::ClipboardTypes(transfer) => Self::new_array(transfer_items(&transfer).0),
            Builtin::ClipboardItems(transfer) => Self::new_array(transfer_items(&transfer).1),
            Builtin::ClipboardItemConstructor => {
                let mut entries = Vec::new();
                if let source @ Value::Object(_) = first.resolve() {
                    for key in self.own_enumerable_keys(&source)? {
                        let value = self.get_property(&source, &key)?;
                        entries.push((key.to_ascii_lowercase(), value));
                    }
                }
                if entries.is_empty() {
                    return Err(self.type_error(
                        "Failed to construct 'ClipboardItem': Empty dictionary argument",
                    ));
                }
                clipboard_item(Rc::new(entries))
            }
            Builtin::ClipboardItemGetType(entries) => {
                let wanted = first.string_value().to_ascii_lowercase();
                let Some((kind, value)) = entries.iter().find(|(kind, _)| *kind == wanted) else {
                    return Ok(Value::Promise(self.rejected_promise_value(dom_exception(
                        "NotFoundError",
                        "The type was not found",
                    ))));
                };
                let kind = kind.clone();
                match value.resolve() {
                    Value::String(text) => {
                        Value::Promise(self.fulfilled_promise(Value::Blob(Rc::new(BlobData {
                            bytes: text.as_bytes().to_vec(),
                            mime_type: kind,
                        }))))
                    }
                    other => Value::Promise(self.promise_resolve(other)?),
                }
            }
            Builtin::ClipboardRead => {
                let Some(provider) = self.clipboard_provider.clone() else {
                    return Ok(Value::Promise(self.rejected_promise_value(not_allowed(
                        "clipboard access is unavailable",
                    ))));
                };
                let mut entries = Vec::new();
                if let Ok(Some((mime_type, bytes))) = provider.read_image() {
                    entries.push((
                        mime_type.clone(),
                        Value::Blob(Rc::new(BlobData { bytes, mime_type })),
                    ));
                }
                if let Ok(text) = provider.read_text()
                    && !text.is_empty()
                {
                    entries.push(("text/plain".to_string(), Value::String(text.into())));
                }
                let items = if entries.is_empty() {
                    Vec::new()
                } else {
                    vec![clipboard_item(Rc::new(entries))]
                };
                Value::Promise(self.fulfilled_promise(Self::new_array(items)))
            }
            Builtin::ClipboardWrite => self.write_clipboard(&first)?,
            Builtin::ClipboardWriteStep(step) => {
                let (write, index) = *step;
                let bytes = match first.resolve() {
                    Value::Blob(blob) => blob.bytes.clone(),
                    Value::File(file) => file.bytes.clone(),
                    Value::String(text) => text.as_bytes().to_vec(),
                    _ => {
                        let done = write.borrow().done.clone();
                        self.settle_promise(
                            &done,
                            PromiseOutcome::Rejected(not_allowed("a clipboard item held no data")),
                        );
                        return Ok(Value::Undefined);
                    }
                };
                let ready = {
                    let mut write = write.borrow_mut();
                    write.pieces[index].1 = Some(bytes);
                    write.waiting -= 1;
                    write.waiting == 0
                };
                if ready {
                    self.finish_clipboard_write(&write);
                }
                Value::Undefined
            }
            Builtin::ClipboardWriteFailed(write) => {
                let done = write.borrow().done.clone();
                self.settle_promise(&done, PromiseOutcome::Rejected(first));
                Value::Undefined
            }
            _ => Value::Undefined,
        })
    }

    // what the first item holds goes to the clipboard once every promised piece of it is in
    fn write_clipboard(&mut self, items: &Value) -> Result<Value, ScriptError> {
        if self.clipboard_provider.is_none() {
            return Ok(Value::Promise(self.rejected_promise_value(not_allowed(
                "clipboard access is unavailable",
            ))));
        }
        let first = match items.resolve() {
            Value::Array(items) => items.borrow().first().cloned(),
            _ => None,
        };
        let entries = match first.map(|item| self.get_property(&item, "getType")) {
            Some(Ok(Value::Builtin(b))) => match b.as_ref() {
                Builtin::ClipboardItemGetType(entries) => entries.clone(),
                _ => {
                    return Ok(Value::Promise(self.rejected_promise_value(dom_exception(
                        "DataError",
                        "Failed to execute 'write' on 'Clipboard': No ClipboardItem was given",
                    ))));
                }
            },
            _ => {
                return Ok(Value::Promise(self.rejected_promise_value(dom_exception(
                    "DataError",
                    "Failed to execute 'write' on 'Clipboard': No ClipboardItem was given",
                ))));
            }
        };
        let done = Self::pending_promise();
        let write = Rc::new(RefCell::new(Write {
            pieces: entries
                .iter()
                .map(|(kind, _)| (kind.clone(), None))
                .collect(),
            waiting: entries.len(),
            done: done.clone(),
        }));
        for (index, (kind, value)) in entries.iter().enumerate() {
            let piece = match value.resolve() {
                Value::String(text) => Value::Blob(Rc::new(BlobData {
                    bytes: text.as_bytes().to_vec(),
                    mime_type: kind.clone(),
                })),
                other => other,
            };
            let promise = self.promise_resolve(piece)?;
            self.promise_then(
                promise,
                Some(&Value::builtin(Builtin::ClipboardWriteStep(Box::new((
                    write.clone(),
                    index,
                ))))),
                Some(&Value::builtin(Builtin::ClipboardWriteFailed(
                    write.clone(),
                ))),
            )?;
        }
        Ok(Value::Promise(done))
    }

    // a picture wins over text, as only one kind fits the host clipboard at once here
    fn finish_clipboard_write(&mut self, write: &WriteHandle) {
        let (pieces, done) = {
            let write = write.borrow();
            (write.pieces.clone(), write.done.clone())
        };
        let Some(provider) = self.clipboard_provider.clone() else {
            return;
        };
        let image = pieces
            .iter()
            .find(|(kind, bytes)| kind.starts_with("image/") && bytes.is_some());
        let text = pieces
            .iter()
            .find(|(kind, bytes)| kind == "text/plain" && bytes.is_some());
        let outcome = match (image, text) {
            (Some((kind, Some(bytes))), _) => provider.write_image(kind, bytes),
            (_, Some((_, Some(bytes)))) => provider.write_text(&String::from_utf8_lossy(bytes)),
            _ => Err(crate::navigation::NavigationError::Clipboard(
                "no clipboard item type this host can write".to_string(),
            )),
        };
        let outcome = match outcome {
            Ok(()) => PromiseOutcome::Fulfilled(Value::Undefined),
            Err(error) => PromiseOutcome::Rejected(not_allowed(&error.to_string())),
        };
        self.settle_promise(&done, outcome);
    }
}

// a ClipboardItem: its types, and getType for each
fn clipboard_item(entries: ItemHandle) -> Value {
    let types = entries
        .iter()
        .map(|(kind, _)| Value::String(kind.clone().into()))
        .collect();
    plain_object([
        ("types".to_string(), ScriptRuntime::new_array(types)),
        (
            "presentationStyle".to_string(),
            Value::String("unspecified".into()),
        ),
        (
            "getType".to_string(),
            Value::builtin(Builtin::ClipboardItemGetType(entries)),
        ),
    ])
}

// a DataTransfer over a transfer's data: types and items read what it holds now, so data set
// by a handler or on one script made shows at once
pub(super) fn transfer_items(handle: &TransferHandle) -> (Vec<Value>, Vec<Value>) {
    let text = |value: &str| Value::String(value.to_string().into());
    let item = |kind: &str, mime: &str, string: Option<&str>, file: Option<&FileHandle>| {
        plain_object([
            ("kind".to_string(), text(kind)),
            ("type".to_string(), text(mime)),
            (
                "getAsString".to_string(),
                Value::builtin(Builtin::ClipboardItemString(Box::new(string.map(Rc::from)))),
            ),
            (
                "getAsFile".to_string(),
                Value::builtin(Builtin::ClipboardItemFile(file.cloned())),
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
    if !transfer.files.is_empty() {
        types.push(text("Files"));
    }
    items.extend(
        transfer
            .files
            .iter()
            .map(|file| item("file", &file.mime_type, None, Some(file))),
    );
    (types, items)
}

impl ScriptRuntime {
    pub(super) fn transfer_object(&self, handle: &TransferHandle) -> Value {
        let text = |value: &str| Value::String(value.to_string().into());
        let method = |builtin: Builtin| Value::builtin(builtin);
        let files = handle.borrow().files.clone();
        let transfer = plain_object([
            (
                "files".to_string(),
                ScriptRuntime::new_array(files.into_iter().map(Value::File).collect()),
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
        ]);
        if let Value::Object(object) = &transfer {
            let mut object = object.borrow_mut();
            for (name, getter) in [
                ("types", Builtin::ClipboardTypes(handle.clone())),
                ("items", Builtin::ClipboardItems(handle.clone())),
            ] {
                object.define(
                    name.to_string(),
                    object::Property {
                        slot: object::Slot::accessor(Some(Value::builtin(getter)), None),
                        enumerable: true,
                        writable: false,
                        configurable: true,
                    },
                );
            }
        }
        self.host_object(transfer, "DataTransfer")
    }

    // new DataTransfer(): an empty transfer that script may write
    pub(super) fn new_data_transfer(&self) -> Value {
        self.transfer_object(&Rc::new(RefCell::new(Transfer {
            data: Vec::new(),
            files: Vec::new(),
            writable: true,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_data_transfer_script_makes_holds_what_it_sets() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const transfer = new DataTransfer();
                const seen = [transfer instanceof DataTransfer, transfer.types.length];
                transfer.setData("text", "plain words");
                transfer.setData("text/html", "<b>bold</b>");
                seen.push(transfer.types.join(), transfer.getData("text/plain"), transfer.items.length);
                transfer.clearData("text/html");
                seen.push(transfer.types.join(), Object.prototype.toString.call(transfer));
                try { DataTransfer(); } catch (error) { seen.push(error.name); }
                document.getElementById("out").textContent = seen.join("|");
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(
                "true|0|text/plain,text/html|plain words|2|text/plain|[object DataTransfer]|TypeError"
            )
        );
    }
}
