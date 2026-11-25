// custom elements: the registry, construction through HTMLElement, upgrades, and the
// connected, disconnected and attribute-changed reactions run after each DOM call

use super::operations::{is_callable, is_primitive};
use super::realm::Native;
use super::*;
use crate::dom::Mutation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RegistryMethod {
    Define,
    Get,
    GetName,
    WhenDefined,
    Upgrade,
}

pub(super) struct Definition {
    name: String,
    constructor: Value,
    observed: Vec<String>,
    connected: Value,
    disconnected: Value,
    attribute_changed: Value,
}

impl Definition {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        tracer.trace_value(&self.constructor);
        tracer.trace_value(&self.connected);
        tracer.trace_value(&self.disconnected);
        tracer.trace_value(&self.attribute_changed);
    }
}

// names the HTML standard reserves even though they contain a hyphen
const RESERVED: &[&str] = &[
    "annotation-xml",
    "color-profile",
    "font-face",
    "font-face-src",
    "font-face-uri",
    "font-face-format",
    "font-face-name",
    "missing-glyph",
];

fn valid_custom_element_name(name: &str) -> bool {
    name.starts_with(|first: char| first.is_ascii_lowercase())
        && name.contains('-')
        && !name.chars().any(|character| character.is_ascii_uppercase())
        && !RESERVED.contains(&name)
}

impl ScriptRuntime {
    pub(super) fn custom_element_registry(&self) -> Value {
        let methods = [
            ("define", RegistryMethod::Define, 2),
            ("get", RegistryMethod::Get, 1),
            ("getName", RegistryMethod::GetName, 1),
            ("whenDefined", RegistryMethod::WhenDefined, 1),
            ("upgrade", RegistryMethod::Upgrade, 1),
        ];
        super::operations::plain_object(methods.into_iter().map(|(name, method, length)| {
            (
                name.to_string(),
                self.native_function(Native::CustomElements(method), name, length),
            )
        }))
    }

    fn definition_index(&self, name: &str) -> Option<usize> {
        self.custom_definitions
            .iter()
            .position(|definition| definition.name == name)
    }

    fn definition_for_constructor(&self, constructor: &Value) -> Option<usize> {
        self.custom_definitions
            .iter()
            .position(|definition| values_equal(&definition.constructor, constructor))
    }

    pub(super) fn call_registry(
        &mut self,
        method: RegistryMethod,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        match method {
            RegistryMethod::Define => {
                let name = self.coerce_string(first)?;
                let constructor = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                self.define_custom_element(name, constructor)?;
                Ok(Value::Undefined)
            }
            RegistryMethod::Get => {
                let name = self.coerce_string(first)?;
                Ok(self
                    .definition_index(&name)
                    .map_or(Value::Undefined, |index| {
                        self.custom_definitions[index].constructor.clone()
                    }))
            }
            RegistryMethod::GetName => Ok(self
                .definition_for_constructor(&first)
                .map_or(Value::Null, |index| {
                    Value::String(self.custom_definitions[index].name.clone().into())
                })),
            RegistryMethod::WhenDefined => {
                let name = self.coerce_string(first)?;
                if !valid_custom_element_name(&name) {
                    return Ok(Value::Promise(self.rejected_promise_value(dom_exception(
                        "SyntaxError",
                        &format!("\"{name}\" is not a valid custom element name"),
                    ))));
                }
                if let Some(index) = self.definition_index(&name) {
                    let constructor = self.custom_definitions[index].constructor.clone();
                    return Ok(Value::Promise(self.fulfilled_promise(constructor)));
                }
                let promise = self
                    .when_defined
                    .entry(name)
                    .or_insert_with(Self::pending_promise)
                    .clone();
                Ok(Value::Promise(promise))
            }
            RegistryMethod::Upgrade => {
                let root = match first.resolve() {
                    Value::Element(handle) => handle.id,
                    Value::Document => self.document_node(),
                    _ => {
                        return Err(self.type_error(
                            "Failed to execute 'upgrade' on 'CustomElementRegistry': parameter 1 is not of type 'Node'.",
                        ));
                    }
                };
                let nodes = {
                    let document = self.document.borrow();
                    std::iter::once(root)
                        .chain(document.descendants(root))
                        .filter(|&node| document.element(node).is_some())
                        .collect::<Vec<_>>()
                };
                for node in nodes {
                    self.try_upgrade(node)?;
                }
                Ok(Value::Undefined)
            }
        }
    }

    fn define_custom_element(
        &mut self,
        name: String,
        constructor: Value,
    ) -> Result<(), ScriptError> {
        if !is_callable(&constructor) {
            return Err(self.type_error(
                "Failed to execute 'define' on 'CustomElementRegistry': The provided value is not a constructor.",
            ));
        }
        if !valid_custom_element_name(&name) {
            return Err(self.throw_value(dom_exception(
                "SyntaxError",
                &format!("Failed to execute 'define' on 'CustomElementRegistry': \"{name}\" is not a valid custom element name"),
            )));
        }
        if self.definition_index(&name).is_some() {
            return Err(self.throw_value(dom_exception(
                "NotSupportedError",
                &format!("Failed to execute 'define' on 'CustomElementRegistry': the name \"{name}\" has already been used with this registry"),
            )));
        }
        if self.definition_for_constructor(&constructor).is_some() {
            return Err(self.throw_value(dom_exception(
                "NotSupportedError",
                "Failed to execute 'define' on 'CustomElementRegistry': this constructor has already been used with this registry",
            )));
        }
        let prototype = self.get_property(&constructor, "prototype")?;
        if is_primitive(&prototype) {
            return Err(
                self.type_error("The prototype of a custom element constructor must be an object")
            );
        }
        let callback = |runtime: &mut Self, name: &str| -> Result<Value, ScriptError> {
            let value = runtime.get_property(&prototype, name)?;
            if matches!(value, Value::Undefined) || is_callable(&value) {
                Ok(value)
            } else {
                Err(runtime.type_error(format!("The '{name}' callback is not a function")))
            }
        };
        let connected = callback(self, "connectedCallback")?;
        let disconnected = callback(self, "disconnectedCallback")?;
        let attribute_changed = callback(self, "attributeChangedCallback")?;
        let mut observed = Vec::new();
        if !matches!(attribute_changed, Value::Undefined) {
            let list = self.get_property(&constructor, "observedAttributes")?;
            if !matches!(list, Value::Undefined) {
                let mut names = Vec::new();
                self.for_each_iterated(list, |runtime, value| {
                    names.push(runtime.coerce_string(value)?);
                    Ok(())
                })?;
                observed = names;
            }
        }
        self.custom_definitions.push(Definition {
            name: name.clone(),
            constructor: constructor.clone(),
            observed,
            connected,
            disconnected,
            attribute_changed,
        });
        self.document.borrow_mut().record_element_changes(true);
        let candidates = {
            let document = self.document.borrow();
            document
                .descendants(document.root())
                .filter(|&node| {
                    document
                        .element(node)
                        .is_some_and(|element| element.name == name)
                })
                .collect::<Vec<_>>()
        };
        for node in candidates {
            self.try_upgrade(node)?;
        }
        if let Some(promise) = self.when_defined.remove(&name) {
            self.resolve_promise(&promise, constructor)?;
        }
        Ok(())
    }

    // the HTMLElement constructor as a custom element's super(): the element being upgraded,
    // or a new element named by the definition
    pub(super) fn construct_custom_element(
        &mut self,
        new_target: &Value,
    ) -> Result<Value, ScriptError> {
        let Some(index) = self.definition_for_constructor(new_target) else {
            return Err(self.type_error("Illegal constructor"));
        };
        let prototype = self.prototype_from_constructor(new_target)?;
        let node = match self.construction_stack.last().copied().flatten() {
            Some(node) => {
                // later super() calls for the same upgrade find the marker and fail
                *self.construction_stack.last_mut().expect("checked above") = None;
                node
            }
            None if !self.construction_stack.is_empty() => {
                return Err(self.throw_value(dom_exception(
                    "InvalidStateError",
                    "This element has already been constructed.",
                )));
            }
            None => {
                let name = self.custom_definitions[index].name.clone();
                self.document.borrow_mut().create_element(&name)
            }
        };
        self.node_handle(node)
            .object
            .borrow_mut()
            .set_prototype(Some(prototype));
        self.custom_states.insert(node, index);
        Ok(self.node_value(node))
    }

    // document.createElement for a defined name runs the constructor right away
    pub(super) fn create_custom_element(
        &mut self,
        name: &str,
    ) -> Result<Option<Value>, ScriptError> {
        let Some(index) = self.definition_index(name) else {
            return Ok(None);
        };
        let constructor = self.custom_definitions[index].constructor.clone();
        let element = self.construct(constructor.clone(), Vec::new(), Some(constructor))?;
        match element.resolve() {
            Value::Element(handle)
                if self
                    .document
                    .borrow()
                    .element(handle.id)
                    .is_some_and(|element| element.name == name) =>
            {
                Ok(Some(Value::Element(handle)))
            }
            _ => Err(self.throw_value(dom_exception(
                "NotSupportedError",
                "The result of constructing a custom element must be a new element with its name.",
            ))),
        }
    }

    // upgrades an element whose name has a definition and that is not custom yet
    fn try_upgrade(&mut self, node: NodeId) -> Result<(), ScriptError> {
        if self.custom_states.contains_key(&node) {
            return Ok(());
        }
        let name = match self.document.borrow().element(node) {
            Some(element) => element.name.clone(),
            None => return Ok(()),
        };
        let Some(index) = self.definition_index(&name) else {
            return Ok(());
        };
        let constructor = self.custom_definitions[index].constructor.clone();
        self.construction_stack.push(Some(node));
        let constructed = self.construct(constructor.clone(), Vec::new(), Some(constructor));
        self.construction_stack.pop();
        if let Err(error) = constructed {
            self.custom_states.remove(&node);
            self.report_exception(error);
            return Ok(());
        }
        self.custom_states.insert(node, index);
        let attributes = self
            .document
            .borrow()
            .element(node)
            .map(|element| {
                element
                    .attrs
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (attribute, value) in attributes {
            self.attribute_changed(node, &attribute, None, Some(value))?;
        }
        if self.document.borrow().is_connected(node) {
            self.lifecycle(node, |definition| definition.connected.clone(), Vec::new())?;
        }
        Ok(())
    }

    fn attribute_changed(
        &mut self,
        node: NodeId,
        name: &str,
        old: Option<String>,
        new: Option<String>,
    ) -> Result<(), ScriptError> {
        let Some(&index) = self.custom_states.get(&node) else {
            return Ok(());
        };
        if !self.custom_definitions[index]
            .observed
            .iter()
            .any(|observed| observed == name)
        {
            return Ok(());
        }
        let text =
            |value: Option<String>| value.map_or(Value::Null, |value| Value::String(value.into()));
        let arguments = vec![
            Value::String(name.into()),
            text(old),
            text(new),
            Value::Null,
        ];
        self.lifecycle(
            node,
            |definition| definition.attribute_changed.clone(),
            arguments,
        )
    }

    fn lifecycle(
        &mut self,
        node: NodeId,
        callback: impl Fn(&Definition) -> Value,
        arguments: Vec<Value>,
    ) -> Result<(), ScriptError> {
        let Some(&index) = self.custom_states.get(&node) else {
            return Ok(());
        };
        let callback = callback(&self.custom_definitions[index]);
        if matches!(callback, Value::Undefined) {
            return Ok(());
        }
        let element = self.node_value(node);
        match self.call_function(callback, element, arguments) {
            Ok(_) => Ok(()),
            Err(error) if self.interrupted => Err(error),
            Err(error) => {
                self.report_exception(error);
                Ok(())
            }
        }
    }

    // the custom element reactions for every change since the last DOM call returned
    pub(super) fn run_element_reactions(&mut self) -> Result<(), ScriptError> {
        loop {
            let entries = self.document.borrow_mut().take_element_changes();
            if entries.is_empty() {
                return Ok(());
            }
            for entry in entries {
                match entry.mutation {
                    Mutation::ChildList { added, removed, .. } => {
                        let root = self.document.borrow().root();
                        let was_connected = entry.target == root || entry.ancestors.contains(&root);
                        for node in removed {
                            if !was_connected || self.document.borrow().is_connected(node) {
                                continue;
                            }
                            for element in self.custom_subtree(node) {
                                self.lifecycle(
                                    element,
                                    |definition| definition.disconnected.clone(),
                                    Vec::new(),
                                )?;
                            }
                        }
                        for node in added {
                            let elements = {
                                let document = self.document.borrow();
                                if !document.contains(node) {
                                    continue;
                                }
                                std::iter::once(node)
                                    .chain(document.descendants(node))
                                    .filter(|&node| document.element(node).is_some())
                                    .collect::<Vec<_>>()
                            };
                            for element in elements {
                                if self.custom_states.contains_key(&element) {
                                    if self.document.borrow().is_connected(element) {
                                        self.lifecycle(
                                            element,
                                            |definition| definition.connected.clone(),
                                            Vec::new(),
                                        )?;
                                    }
                                } else {
                                    self.try_upgrade(element)?;
                                }
                            }
                        }
                    }
                    Mutation::Attribute { name, old } => {
                        let new = self
                            .document
                            .borrow()
                            .attribute(entry.target, &name)
                            .map(str::to_string);
                        self.attribute_changed(entry.target, &name, old, new)?;
                    }
                    Mutation::CharacterData { .. } => {}
                }
            }
        }
    }

    fn custom_subtree(&self, node: NodeId) -> Vec<NodeId> {
        let document = self.document.borrow();
        if !document.contains(node) {
            return Vec::new();
        }
        std::iter::once(node)
            .chain(document.descendants(node))
            .filter(|node| self.custom_states.contains_key(node))
            .collect()
    }
}
