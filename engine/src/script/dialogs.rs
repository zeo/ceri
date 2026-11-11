// dialog elements: show, showModal and close, the top layer modal dialogs are drawn in, and the
// cancel escape asks for before a modal dialog closes

use super::interfaces::Access;
use super::realm::Native;
use super::*;

#[derive(Default)]
pub(super) struct DialogState {
    return_value: String,
    // where focus goes back to when the dialog closes
    previous_focus: Option<NodeId>,
}

impl ScriptRuntime {
    pub(super) fn dialog_member(
        &mut self,
        handle: &ElementHandle,
        name: &str,
        access: Access,
        first: Value,
    ) -> Result<Value, ScriptError> {
        let node = handle.id;
        match (name, access) {
            ("returnValue", Access::Get) => Ok(Value::String(
                self.dialogs
                    .get(&node)
                    .map(|dialog| dialog.return_value.clone())
                    .unwrap_or_default()
                    .into(),
            )),
            ("returnValue", _) => {
                let value = self.coerce_string(first)?;
                self.dialogs.entry(node).or_default().return_value = value;
                Ok(Value::Undefined)
            }
            ("showModal", _) => self.show_dialog(handle, true),
            ("show", _) => self.show_dialog(handle, false),
            ("close", _) => {
                let result = match first {
                    Value::Undefined => None,
                    value => Some(self.coerce_string(value)?),
                };
                self.close_dialog(node, result)?;
                Ok(Value::Undefined)
            }
            _ => {
                let result = match first {
                    Value::Undefined => None,
                    value => Some(self.coerce_string(value)?),
                };
                self.request_dialog_close(node, result)?;
                Ok(Value::Undefined)
            }
        }
    }

    // the connected open modal dialogs, bottom first
    pub(super) fn modal_dialogs(&self) -> Vec<NodeId> {
        let document = self.document.borrow();
        self.top_layer
            .iter()
            .copied()
            .filter(|&node| {
                document.is_connected(node) && document.attribute(node, "open").is_some()
            })
            .collect()
    }

    fn dialog_open(&self, node: NodeId) -> bool {
        self.document.borrow().attribute(node, "open").is_some()
    }

    fn show_dialog(&mut self, handle: &ElementHandle, modal: bool) -> Result<Value, ScriptError> {
        let node = handle.id;
        let is_modal = self.modal_dialogs().contains(&node);
        if self.dialog_open(node) {
            if is_modal == modal {
                return Ok(Value::Undefined);
            }
            let message = match modal {
                true => {
                    "The element already has an 'open' attribute, and therefore cannot be opened modally."
                }
                false => {
                    "The dialog is already open as a modal dialog, and therefore cannot be opened as a non-modal dialog."
                }
            };
            return Err(self.throw_value(dom_exception("InvalidStateError", message)));
        }
        if modal && !self.document.borrow().is_connected(node) {
            return Err(self.throw_value(dom_exception(
                "InvalidStateError",
                "The element is not in a Document.",
            )));
        }
        self.set_element_attribute(handle, "open", String::new());
        if modal {
            self.top_layer.retain(|&entry| entry != node);
            self.top_layer.push(node);
        }
        self.dialogs.entry(node).or_default().previous_focus = self.focused_element;
        self.focus_dialog(node)?;
        Ok(Value::Undefined)
    }

    // the dialog's autofocus descendant, else its first focusable one, else the dialog itself
    fn focus_dialog(&mut self, dialog: NodeId) -> Result<(), ScriptError> {
        let descendants = {
            let document = self.document.borrow();
            match document.attribute(dialog, "autofocus") {
                Some(_) => Vec::new(),
                None => document.descendants(dialog).collect::<Vec<_>>(),
            }
        };
        let focusable = descendants
            .iter()
            .copied()
            .filter(|&node| self.is_focusable(node))
            .collect::<Vec<_>>();
        let target = focusable
            .iter()
            .copied()
            .find(|&node| {
                self.document
                    .borrow()
                    .attribute(node, "autofocus")
                    .is_some()
            })
            .or(focusable.first().copied())
            .unwrap_or(dialog);
        self.move_focus(target)?;
        Ok(())
    }

    pub(super) fn close_dialog(
        &mut self,
        node: NodeId,
        result: Option<String>,
    ) -> Result<(), ScriptError> {
        if !self.dialog_open(node) {
            return Ok(());
        }
        let handle = self.node_handle(node);
        self.remove_element_attribute(&handle, "open");
        let was_modal = self.top_layer.contains(&node);
        self.top_layer.retain(|&entry| entry != node);
        let dialog = self.dialogs.entry(node).or_default();
        if let Some(result) = result {
            dialog.return_value = result;
        }
        let previous = dialog.previous_focus.take();
        let focus_inside = self
            .focused_element
            .is_some_and(|focused| self.document.borrow().is_inclusive_ancestor(node, focused));
        if was_modal || focus_inside {
            let restored = match previous {
                Some(previous) if self.document.borrow().is_connected(previous) => {
                    self.focus_node(previous)?
                }
                _ => false,
            };
            // a closed dialog isn't rendered, so focus inside it falls back to the body
            if !restored && focus_inside {
                self.blur_focused()?;
            }
        }
        self.schedule_native_task(Native::DialogClose(node));
        Ok(())
    }

    pub(super) fn deliver_dialog_close(&mut self, node: NodeId) -> Result<Value, ScriptError> {
        let event = self.new_event("Event", "close".to_string(), false, false, false, true);
        self.dispatch(&event, events::ListenerTarget::Node(node))?;
        Ok(Value::Undefined)
    }

    // a cancel that script can refuse only after a user activation it hasn't spent on one yet
    pub(super) fn request_dialog_close(
        &mut self,
        node: NodeId,
        result: Option<String>,
    ) -> Result<(), ScriptError> {
        if !self.dialog_open(node) {
            return Ok(());
        }
        let cancelable = std::mem::take(&mut self.history_activation);
        let event = self.new_event(
            "Event",
            "cancel".to_string(),
            false,
            cancelable,
            false,
            true,
        );
        self.dispatch(&event, events::ListenerTarget::Node(node))?;
        if !event.canceled() {
            self.close_dialog(node, result)?;
        }
        Ok(())
    }
}
