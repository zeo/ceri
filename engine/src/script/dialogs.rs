// dialog elements: show, showModal and close, the top layer modal dialogs are drawn in, and the
// cancel escape asks for before a modal dialog closes; and popovers, which share that layer

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
                document.is_connected(node)
                    && document.attribute(node, "open").is_some()
                    && !self.popovers.contains(&node)
            })
            .collect()
    }

    // what the top layer draws, bottom first: open modal dialogs and popovers on show
    pub(super) fn top_layer_elements(&self) -> Vec<NodeId> {
        let document = self.document.borrow();
        self.top_layer
            .iter()
            .copied()
            .filter(|&node| {
                document.is_connected(node)
                    && (self.popovers.contains(&node) || document.attribute(node, "open").is_some())
            })
            .collect()
    }

    // the popover attribute's state: auto, manual or hint, none without the attribute
    fn popover_kind(&self, node: NodeId) -> Option<&'static str> {
        let value = self
            .document
            .borrow()
            .attribute(node, "popover")?
            .to_ascii_lowercase();
        Some(match value.as_str() {
            "" | "auto" => "auto",
            "hint" => "hint",
            _ => "manual",
        })
    }

    // auto and hint popovers close each other and close on escape or a press outside
    fn light_dismissable(&self, node: NodeId) -> bool {
        self.popover_kind(node).is_some_and(|kind| kind != "manual")
    }

    pub(super) fn topmost_auto_popover(&self) -> Option<NodeId> {
        self.popovers
            .iter()
            .rev()
            .copied()
            .find(|&node| self.light_dismissable(node))
    }

    pub(super) fn popover_member(
        &mut self,
        handle: &ElementHandle,
        name: &str,
        access: Access,
        first: Value,
    ) -> Result<Value, ScriptError> {
        let node = handle.id;
        Ok(match (name, access) {
            ("popover", Access::Get) => self
                .popover_kind(node)
                .map_or(Value::Null, |kind| Value::String(kind.into())),
            ("popover", _) => {
                if matches!(first, Value::Null) {
                    self.remove_element_attribute(handle, "popover");
                } else {
                    let value = self.coerce_string(first)?;
                    self.set_element_attribute(handle, "popover", value);
                }
                Value::Undefined
            }
            ("popoverTargetAction", Access::Get) => {
                Value::String(self.popover_target_action(node).into())
            }
            ("popoverTargetAction", _) => {
                let value = self.coerce_string(first)?;
                self.set_element_attribute(handle, "popovertargetaction", value);
                Value::Undefined
            }
            ("showPopover", _) => {
                self.show_popover(node)?;
                Value::Undefined
            }
            ("hidePopover", _) => {
                self.check_popover(node)?;
                self.hide_popover(node, true)?;
                Value::Undefined
            }
            _ => {
                // togglePopover takes a boolean or { force }
                let force = match first {
                    Value::Undefined => None,
                    options if !super::operations::is_primitive(&options) => {
                        match self.get_property(&options, "force")? {
                            Value::Undefined => None,
                            force => Some(force.truthy()),
                        }
                    }
                    force => Some(force.truthy()),
                };
                let showing = self.popovers.contains(&node);
                match force.unwrap_or(!showing) {
                    true if !showing => self.show_popover(node)?,
                    false if showing => self.hide_popover(node, true)?,
                    _ => self.check_popover(node)?,
                }
                Value::Bool(self.popovers.contains(&node))
            }
        })
    }

    fn popover_target_action(&self, node: NodeId) -> &'static str {
        match self
            .document
            .borrow()
            .attribute(node, "popovertargetaction")
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("show") => "show",
            Some("hide") => "hide",
            _ => "toggle",
        }
    }

    fn check_popover(&mut self, node: NodeId) -> Result<(), ScriptError> {
        if self.popover_kind(node).is_none() {
            return Err(self.throw_value(dom_exception(
                "NotSupportedError",
                "Not supported on elements that do not have a valid value for the 'popover' attribute.",
            )));
        }
        Ok(())
    }

    fn toggle_event(
        &mut self,
        node: NodeId,
        name: &str,
        opening: bool,
    ) -> Result<bool, ScriptError> {
        let (old, new) = if opening {
            ("closed", "open")
        } else {
            ("open", "closed")
        };
        // only a beforetoggle toward open can be refused
        let cancelable = opening && name == "beforetoggle";
        let event = self.new_event(
            "ToggleEvent",
            name.to_string(),
            false,
            cancelable,
            false,
            true,
        );
        event.set_field("oldState", Value::String(old.into()));
        event.set_field("newState", Value::String(new.into()));
        self.dispatch(&event, events::ListenerTarget::Node(node))?;
        Ok(!event.canceled())
    }

    fn show_popover(&mut self, node: NodeId) -> Result<(), ScriptError> {
        self.check_popover(node)?;
        if self.popovers.contains(&node) {
            return Ok(());
        }
        if !self.document.borrow().is_connected(node) {
            return Err(self.throw_value(dom_exception(
                "InvalidStateError",
                "Invalid on disconnected popover elements.",
            )));
        }
        if self.modal_dialogs().contains(&node) {
            return Err(self.throw_value(dom_exception(
                "InvalidStateError",
                "Invalid on popover elements that are open modal dialogs.",
            )));
        }
        if !self.toggle_event(node, "beforetoggle", true)? {
            return Ok(());
        }
        // a listener may have shown it, removed it or taken the attribute away
        if self.popovers.contains(&node)
            || self.popover_kind(node).is_none()
            || !self.document.borrow().is_connected(node)
        {
            return Ok(());
        }
        if self.light_dismissable(node) {
            // auto popovers that do not hold this one close first, newest first
            let closing = self
                .popovers
                .iter()
                .rev()
                .copied()
                .filter(|&open| {
                    self.light_dismissable(open)
                        && !self.document.borrow().is_inclusive_ancestor(open, node)
                })
                .collect::<Vec<_>>();
            for open in closing {
                self.hide_popover(open, true)?;
            }
        }
        self.popovers.push(node);
        self.top_layer.retain(|&entry| entry != node);
        self.top_layer.push(node);
        self.queue_popover_toggle(node, false);
        Ok(())
    }

    pub(super) fn hide_popover(&mut self, node: NodeId, events: bool) -> Result<(), ScriptError> {
        let Some(index) = self.popovers.iter().position(|&open| open == node) else {
            return Ok(());
        };
        // an auto popover takes the auto popovers shown above it down first
        if self.light_dismissable(node) {
            let above = self.popovers[index + 1..]
                .iter()
                .rev()
                .copied()
                .filter(|&open| self.light_dismissable(open))
                .collect::<Vec<_>>();
            for open in above {
                self.hide_popover(open, events)?;
            }
        }
        if events {
            self.toggle_event(node, "beforetoggle", false)?;
        }
        self.popovers.retain(|&open| open != node);
        self.top_layer.retain(|&entry| entry != node);
        if events {
            self.queue_popover_toggle(node, true);
        }
        Ok(())
    }

    // one toggle event per popover, from the state its first change left to the one it is in
    // when the event fires; a later change moves the event behind it, as browsers coalesce them
    fn queue_popover_toggle(&mut self, node: NodeId, was_open: bool) {
        self.popover_toggles.entry(node).or_insert((was_open, 0)).1 += 1;
        self.schedule_native_task(Native::PopoverToggle(node));
    }

    pub(super) fn deliver_popover_toggle(&mut self, node: NodeId) -> Result<Value, ScriptError> {
        let Some(pending) = self.popover_toggles.get_mut(&node) else {
            return Ok(Value::Undefined);
        };
        pending.1 -= 1;
        if pending.1 > 0 {
            return Ok(Value::Undefined);
        }
        let Some((was_open, _)) = self.popover_toggles.remove(&node) else {
            return Ok(Value::Undefined);
        };
        let state = |open: bool| if open { "open" } else { "closed" };
        let event = self.new_event(
            "ToggleEvent",
            "toggle".to_string(),
            false,
            false,
            false,
            true,
        );
        event.set_field("oldState", Value::String(state(was_open).into()));
        event.set_field(
            "newState",
            Value::String(state(self.popovers.contains(&node)).into()),
        );
        self.dispatch(&event, events::ListenerTarget::Node(node))?;
        Ok(Value::Undefined)
    }

    // the popover a button names in popovertarget, when the button is not submitting a form
    fn popover_target(&self, node: NodeId) -> Option<NodeId> {
        let document = self.document.borrow();
        let element = document.element(node)?;
        let kind = element.attr("type").map(str::to_ascii_lowercase);
        let invoker = match element.name.as_str() {
            "button" => true,
            "input" => matches!(
                kind.as_deref(),
                Some("button" | "submit" | "reset" | "image")
            ),
            _ => false,
        };
        let id = element.attr("popovertarget").filter(|_| invoker)?;
        let target = document.find_element_by_id(id)?;
        drop(document);
        (self.popover_kind(target).is_some() && self.submit_form_for_click(node).is_none())
            .then_some(target)
    }

    pub(super) fn invoke_popover_target(&mut self, node: NodeId) -> Result<(), ScriptError> {
        let Some(target) = self.popover_target(node) else {
            return Ok(());
        };
        let showing = self.popovers.contains(&target);
        match self.popover_target_action(node) {
            "show" if !showing => self.show_popover(target),
            "hide" if showing => self.hide_popover(target, true),
            "toggle" if showing => self.hide_popover(target, true),
            "toggle" => self.show_popover(target),
            _ => Ok(()),
        }
    }

    // a press outside the auto popovers closes the ones above the popover it landed in, where
    // a button that opens a popover counts as inside it
    pub(crate) fn light_dismiss_popovers(&mut self, target: NodeId) -> Result<(), ScriptError> {
        if self.topmost_auto_popover().is_none() {
            return Ok(());
        }
        let ancestry = {
            let document = self.document.borrow();
            std::iter::once(target)
                .chain(document.ancestors(target))
                .collect::<Vec<_>>()
        };
        let holder = ancestry.iter().find_map(|&node| {
            if self.popovers.contains(&node) && self.light_dismissable(node) {
                return Some(node);
            }
            self.popover_target(node)
                .filter(|popover| self.popovers.contains(popover))
        });
        let floor = holder
            .and_then(|holder| self.popovers.iter().position(|&open| open == holder))
            .map_or(0, |index| index + 1);
        let closing = self.popovers[floor..]
            .iter()
            .copied()
            .find(|&open| self.light_dismissable(open));
        if let Some(lowest) = closing {
            self.hide_popover(lowest, true)?;
            self.drain_microtasks()?;
        }
        Ok(())
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
