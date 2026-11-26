// Proxy and Reflect: a proxy forwards each internal operation to a handler trap when the
// handler defines one and to its target otherwise; Reflect exposes the same operations

use super::object::{
    JsObject, ObjectClass, ObjectHandle, Property, Slot, is_symbol_key, new_object,
};
use super::operations::{is_callable, is_primitive, same_value};
use super::*;

pub(super) struct ProxyData {
    pub(super) target: RefCell<Option<Value>>,
    pub(super) handler: RefCell<Option<Value>>,
    pub(super) callable: bool,
    pub(super) constructor: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum ReflectMethod {
    ProxyConstructor,
    ProxyRevocable,
    Apply,
    Construct,
    DefineProperty,
    DeleteProperty,
    Get,
    GetOwnPropertyDescriptor,
    GetPrototypeOf,
    Has,
    IsExtensible,
    OwnKeys,
    PreventExtensions,
    Set,
    SetPrototypeOf,
}

pub(super) const REFLECT_METHODS: &[(&str, ReflectMethod, u32)] = &[
    ("apply", ReflectMethod::Apply, 3),
    ("construct", ReflectMethod::Construct, 2),
    ("defineProperty", ReflectMethod::DefineProperty, 3),
    ("deleteProperty", ReflectMethod::DeleteProperty, 2),
    ("get", ReflectMethod::Get, 2),
    (
        "getOwnPropertyDescriptor",
        ReflectMethod::GetOwnPropertyDescriptor,
        2,
    ),
    ("getPrototypeOf", ReflectMethod::GetPrototypeOf, 1),
    ("has", ReflectMethod::Has, 2),
    ("isExtensible", ReflectMethod::IsExtensible, 1),
    ("ownKeys", ReflectMethod::OwnKeys, 1),
    ("preventExtensions", ReflectMethod::PreventExtensions, 1),
    ("set", ReflectMethod::Set, 3),
    ("setPrototypeOf", ReflectMethod::SetPrototypeOf, 2),
];

fn argument(arguments: &[Value], index: usize) -> Value {
    arguments.get(index).cloned().unwrap_or(Value::Undefined)
}

// a property key as a trap receives it: symbols as symbols, everything else as strings
pub(super) fn key_value(key: &str) -> Value {
    if is_symbol_key(key) {
        Value::Symbol(key.into())
    } else {
        Value::String(key.into())
    }
}

pub(super) fn proxy_of(object: &ObjectHandle) -> Option<Rc<ProxyData>> {
    match &object.borrow().class {
        ObjectClass::Proxy(proxy) => Some(proxy.clone()),
        _ => None,
    }
}

fn same_optional_val(left: &Option<Value>, right: &Option<Value>) -> bool {
    match (left, right) {
        (Some(a), Some(b)) => same_value(a, b),
        (None, None) => true,
        _ => false,
    }
}

fn descriptor_compatible_with_target(
    extensible: bool,
    desc: &Property,
    present: [bool; 6],
    target: Option<&Property>,
) -> bool {
    let Some(current) = target else {
        return extensible;
    };
    if current.configurable {
        return true;
    }
    if present[1] && desc.configurable {
        return false;
    }
    if present[0] && desc.enumerable != current.enumerable {
        return false;
    }
    let desc_is_generic = !(present[2] || present[3] || present[4] || present[5]);
    if desc_is_generic {
        return true;
    }
    let desc_is_accessor = present[4] || present[5];
    let current_is_accessor = matches!(current.slot, Slot::Accessor(_));
    if desc_is_accessor != current_is_accessor {
        return false;
    }
    match (&current.slot, &desc.slot) {
        (Slot::Data(old), Slot::Data(new)) => {
            if !current.writable {
                if present[3] && desc.writable {
                    return false;
                }
                if present[2] && !same_value(old, new) {
                    return false;
                }
            }
            true
        }
        (Slot::Accessor(old), Slot::Accessor(new)) => {
            (!present[4] || same_optional_val(&old.get, &new.get))
                && (!present[5] || same_optional_val(&old.set, &new.set))
        }
        _ => false,
    }
}

impl ScriptRuntime {
    // IsArray, which sees through a proxy to its target
    pub(super) fn is_array(&mut self, value: &Value) -> Result<bool, ScriptError> {
        let proxy = match value {
            Value::Array(_) => return Ok(true),
            Value::Object(object) => proxy_of(object),
            _ => None,
        };
        let Some(proxy) = proxy else {
            return Ok(false);
        };
        let target = proxy.target.borrow().clone();
        match target {
            Some(target) => self.is_array(&target),
            None => {
                Err(self.type_error("Cannot perform 'IsArray' on a proxy that has been revoked"))
            }
        }
    }

    pub(super) fn is_extensible_value(&mut self, value: &Value) -> Result<bool, ScriptError> {
        match value {
            Value::Object(object) => match proxy_of(object) {
                Some(proxy) => {
                    let (target, trap) = self.proxy_trap(&proxy, "isExtensible")?;
                    match trap {
                        Some(trap) => {
                            let trap_result = self
                                .call_function(
                                    trap,
                                    Self::proxy_handler(&proxy),
                                    vec![target.clone()],
                                )?
                                .truthy();
                            let target_result = self.is_extensible_value(&target)?;
                            if trap_result != target_result {
                                return Err(self.type_error(
                                    "'isExtensible' on proxy: trap result does not reflect extensibility of proxy target",
                                ));
                            }
                            Ok(trap_result)
                        }
                        None => self.is_extensible_value(&target),
                    }
                }
                None => Ok(object.borrow().extensible),
            },
            Value::Array(array) => Ok(array.borrow().integrity() == super::Integrity::Open),
            Value::TypedArray(array) => Ok(array
                .borrow()
                .object
                .as_ref()
                .is_none_or(|object| object.borrow().extensible)),
            other => Ok(!is_primitive(other)),
        }
    }

    pub(super) fn prevent_extensions_value(&mut self, value: &Value) -> Result<bool, ScriptError> {
        match value {
            Value::Object(object) => match proxy_of(object) {
                Some(proxy) => {
                    let (target, trap) = self.proxy_trap(&proxy, "preventExtensions")?;
                    match trap {
                        Some(trap) => {
                            let trap_result = self
                                .call_function(
                                    trap,
                                    Self::proxy_handler(&proxy),
                                    vec![target.clone()],
                                )?
                                .truthy();
                            if trap_result && self.is_extensible_value(&target)? {
                                return Err(self.type_error(
                                    "'preventExtensions' on proxy: trap returned truish but the proxy target is extensible",
                                ));
                            }
                            Ok(trap_result)
                        }
                        None => self.prevent_extensions_value(&target),
                    }
                }
                None => {
                    object.borrow_mut().extensible = false;
                    Ok(true)
                }
            },
            Value::Array(array) => {
                let mut array = array.borrow_mut();
                let extra = array.extra_mut();
                extra.integrity = extra.integrity.max(super::Integrity::Closed);
                Ok(true)
            }
            Value::TypedArray(array) => {
                super::typed_object(array).borrow_mut().extensible = false;
                Ok(true)
            }
            _ => Ok(true),
        }
    }

    pub(super) fn proxy_set_prototype(
        &mut self,
        proxy: &ProxyData,
        prototype: Option<ObjectHandle>,
    ) -> Result<bool, ScriptError> {
        let (target, trap) = self.proxy_trap(proxy, "setPrototypeOf")?;
        let value = Self::prototype_value(prototype.clone());
        match trap {
            Some(trap) => {
                let accepted = self
                    .call_function(
                        trap,
                        Self::proxy_handler(proxy),
                        vec![target.clone(), value],
                    )?
                    .truthy();
                if !accepted {
                    return Ok(false);
                }
                if !self.is_extensible_value(&target)? {
                    let target_proto = self.prototype_of_value(&target)?;
                    let same = match (&target_proto, &prototype) {
                        (Some(a), Some(b)) => Self::prototypes_equal(a, b),
                        (None, None) => true,
                        _ => false,
                    };
                    if !same {
                        return Err(self.type_error(
                            "'setPrototypeOf' on proxy: trap returned truish for setting a new prototype on the non-extensible proxy target",
                        ));
                    }
                }
                Ok(true)
            }
            None => match &target {
                Value::Array(array) => Ok(self.set_array_prototype(array, prototype)),
                Value::Object(obj) => {
                    if let Some(inner) = proxy_of(obj) {
                        return self.proxy_set_prototype(&inner, prototype);
                    }
                    match self.set_object_prototype(obj, prototype) {
                        Ok(()) => Ok(true),
                        Err(_) => {
                            self.pending_exception = None;
                            Ok(false)
                        }
                    }
                }
                _ => Ok(true),
            },
        }
    }

    // the target and the named trap of a proxy; a revoked proxy throws
    pub(super) fn proxy_trap(
        &mut self,
        proxy: &ProxyData,
        name: &str,
    ) -> Result<(Value, Option<Value>), ScriptError> {
        let (Some(target), Some(handler)) = (
            proxy.target.borrow().clone(),
            proxy.handler.borrow().clone(),
        ) else {
            return Err(self.type_error(format!(
                "Cannot perform '{name}' on a proxy that has been revoked"
            )));
        };
        let trap = self.get_property(&handler, name)?;
        if matches!(trap, Value::Undefined | Value::Null) {
            return Ok((target, None));
        }
        if !is_callable(&trap) {
            return Err(self.type_error(format!("proxy trap {name} is not a function")));
        }
        Ok((target, Some(trap)))
    }

    fn proxy_handler(proxy: &ProxyData) -> Value {
        proxy.handler.borrow().clone().unwrap_or(Value::Undefined)
    }

    pub(super) fn proxy_get(
        &mut self,
        proxy: &ProxyData,
        key: &str,
        receiver: &Value,
    ) -> Result<Value, ScriptError> {
        match self.proxy_get_tail(proxy, key, receiver)? {
            super::operations::GetOutcome::Value(value) => Ok(value),
            super::operations::GetOutcome::Call {
                callee,
                this,
                arguments,
            } => self.call_function(callee, this, arguments),
        }
    }

    pub(super) fn proxy_get_tail(
        &mut self,
        proxy: &ProxyData,
        key: &str,
        receiver: &Value,
    ) -> Result<super::operations::GetOutcome, ScriptError> {
        let (target, trap) = self.proxy_trap(proxy, "get")?;
        match trap {
            Some(trap) => {
                let result = self.call_function(
                    trap,
                    Self::proxy_handler(proxy),
                    vec![target.clone(), key_value(key), receiver.clone()],
                )?;
                if let Some(target_desc) = self.own_property_of(&target, key)?
                    && !target_desc.configurable
                {
                    match &target_desc.slot {
                        Slot::Data(target_val)
                            if !target_desc.writable && !same_value(&result, target_val) =>
                        {
                            return Err(self.type_error(format!(
                                "'get' on proxy: property '{key}' is a read-only and non-configurable data property on the proxy target"
                            )));
                        }
                        Slot::Accessor(acc)
                            if acc.get.is_none() && !matches!(result, Value::Undefined) =>
                        {
                            return Err(self.type_error(format!(
                                "'get' on proxy: property '{key}' is a non-configurable accessor property without a getter"
                            )));
                        }
                        _ => {}
                    }
                }
                Ok(super::operations::GetOutcome::Value(result))
            }
            None => match &target {
                Value::Object(object) => self.ordinary_get_tail(object.clone(), key, receiver),
                other => self
                    .get_property(other, key)
                    .map(super::operations::GetOutcome::Value),
            },
        }
    }

    pub(super) fn proxy_set_bool(
        &mut self,
        proxy: &ProxyData,
        key: &str,
        value: Value,
        receiver: &Value,
    ) -> Result<bool, ScriptError> {
        let (target, trap) = self.proxy_trap(proxy, "set")?;
        match trap {
            Some(trap) => {
                let accepted = self
                    .call_function(
                        trap,
                        Self::proxy_handler(proxy),
                        vec![
                            target.clone(),
                            key_value(key),
                            value.clone(),
                            receiver.clone(),
                        ],
                    )?
                    .truthy();
                if !accepted {
                    return Ok(false);
                }
                if let Some(target_desc) = self.own_property_of(&target, key)?
                    && !target_desc.configurable
                {
                    match &target_desc.slot {
                        Slot::Data(target_val)
                            if !target_desc.writable && !same_value(&value, target_val) =>
                        {
                            return Err(self.type_error(format!(
                                "'set' on proxy: trap returned truish for property '{key}' which exists in the proxy target as a non-configurable and non-writable data property with a different value"
                            )));
                        }
                        Slot::Accessor(acc) if acc.set.is_none() => {
                            return Err(self.type_error(format!(
                                "'set' on proxy: trap returned truish for property '{key}' which exists in the proxy target as a non-configurable and non-writable accessor property without a setter"
                            )));
                        }
                        _ => {}
                    }
                }
                Ok(true)
            }
            None => match &target {
                Value::Object(object) => {
                    if let Some(inner) = proxy_of(object) {
                        return self.proxy_set_bool(&inner, key, value, receiver);
                    }
                    match self.ordinary_set(object.clone(), key, value, receiver, true) {
                        Ok(()) => Ok(true),
                        Err(error) => {
                            let thrown = self.take_thrown(error);
                            let is_type_error = matches!(&thrown, Value::Object(obj) if obj.borrow().prototype_or(&self.intrinsics).is_some_and(|proto| ObjectHandle::ptr_eq(&proto, &self.intrinsics.type_error_prototype)));
                            if is_type_error {
                                Ok(false)
                            } else {
                                Err(self.throw_value(thrown))
                            }
                        }
                    }
                }
                other => match self.set_property(other, key, value, true) {
                    Ok(()) => Ok(true),
                    Err(error) => {
                        let thrown = self.take_thrown(error);
                        let is_type_error = matches!(&thrown, Value::Object(obj) if obj.borrow().prototype_or(&self.intrinsics).is_some_and(|proto| ObjectHandle::ptr_eq(&proto, &self.intrinsics.type_error_prototype)));
                        if is_type_error {
                            Ok(false)
                        } else {
                            Err(self.throw_value(thrown))
                        }
                    }
                },
            },
        }
    }

    pub(super) fn proxy_set(
        &mut self,
        proxy: &ProxyData,
        key: &str,
        value: Value,
        receiver: &Value,
        strict: bool,
    ) -> Result<(), ScriptError> {
        let accepted = self.proxy_set_bool(proxy, key, value, receiver)?;
        if !accepted && strict {
            return Err(self.type_error(format!(
                "'set' on proxy: trap returned falsish for property '{key}'"
            )));
        }
        Ok(())
    }

    pub(super) fn proxy_has(&mut self, proxy: &ProxyData, key: &str) -> Result<bool, ScriptError> {
        let (target, trap) = self.proxy_trap(proxy, "has")?;
        match trap {
            Some(trap) => {
                let result = self
                    .call_function(
                        trap,
                        Self::proxy_handler(proxy),
                        vec![target.clone(), key_value(key)],
                    )?
                    .truthy();
                if !result
                    && let Some(target_desc) = self.own_property_of(&target, key)?
                    && (!target_desc.configurable || !self.is_extensible_value(&target)?)
                {
                    return Err(self.type_error(format!(
                        "'has' on proxy: trap returned falsish for property '{key}' which exists in the proxy target as non-configurable or non-extensible"
                    )));
                }
                Ok(result)
            }
            None => self.has_property(&target, key),
        }
    }

    pub(super) fn proxy_delete(
        &mut self,
        proxy: &ProxyData,
        key: &str,
        strict: bool,
    ) -> Result<bool, ScriptError> {
        let (target, trap) = self.proxy_trap(proxy, "deleteProperty")?;
        match trap {
            Some(trap) => {
                let deleted = self
                    .call_function(
                        trap,
                        Self::proxy_handler(proxy),
                        vec![target.clone(), key_value(key)],
                    )?
                    .truthy();
                if !deleted {
                    if strict {
                        return Err(self.type_error(format!(
                            "'deleteProperty' on proxy: trap returned falsish for property '{key}'"
                        )));
                    }
                    return Ok(false);
                }
                if let Some(target_desc) = self.own_property_of(&target, key)?
                    && (!target_desc.configurable || !self.is_extensible_value(&target)?)
                {
                    return Err(self.type_error(format!(
                        "'deleteProperty' on proxy: trap returned truish for property '{key}' which is non-configurable or non-extensible in the proxy target"
                    )));
                }
                Ok(true)
            }
            None => self.delete_property(&target, key, strict),
        }
    }

    pub(super) fn proxy_own_keys(&mut self, proxy: &ProxyData) -> Result<Vec<String>, ScriptError> {
        let (target, trap) = self.proxy_trap(proxy, "ownKeys")?;
        match trap {
            Some(trap) => {
                let list =
                    self.call_function(trap, Self::proxy_handler(proxy), vec![target.clone()])?;
                if is_primitive(&list) {
                    return Err(self.type_error("proxy ownKeys trap returned a non-object"));
                }
                let values = self.array_like_list(&list)?;
                let mut keys = Vec::with_capacity(values.len());
                let mut seen = HashSet::default();
                for value in values {
                    let key = match value {
                        Value::String(text) => text.to_string(),
                        Value::Symbol(key) => key.to_string(),
                        _ => return Err(self.type_error("proxy ownKeys trap returned a non-key")),
                    };
                    if !seen.insert(key.clone()) {
                        return Err(self.type_error(format!(
                            "'ownKeys' on proxy: trap returned duplicate entries for '{key}'"
                        )));
                    }
                    keys.push(key);
                }
                let extensible = self.is_extensible_value(&target)?;
                let target_keys = self.all_own_property_keys(&target)?;
                let mut target_non_config = Vec::new();
                let mut target_config = Vec::new();
                for key in target_keys {
                    if let Some(desc) = self.own_property_of(&target, &key)? {
                        if !desc.configurable {
                            target_non_config.push(key);
                        } else {
                            target_config.push(key);
                        }
                    }
                }
                if extensible && target_non_config.is_empty() {
                    return Ok(keys);
                }
                for key in &target_non_config {
                    if !seen.contains(key) {
                        return Err(self.type_error(format!(
                            "'ownKeys' on proxy: trap result did not include non-configurable property '{key}'"
                        )));
                    }
                }
                if extensible {
                    return Ok(keys);
                }
                for key in &target_config {
                    if !seen.contains(key) {
                        return Err(self.type_error(format!(
                            "'ownKeys' on proxy: trap result did not include property '{key}' of non-extensible target"
                        )));
                    }
                }
                if seen.len() != target_non_config.len() + target_config.len() {
                    return Err(self.type_error(
                        "'ownKeys' on proxy: trap returned extra keys but proxy target is non-extensible",
                    ));
                }
                Ok(keys)
            }
            None => self.all_own_property_keys(&target),
        }
    }

    pub(super) fn proxy_own_property(
        &mut self,
        proxy: &ProxyData,
        key: &str,
    ) -> Result<Option<Property>, ScriptError> {
        let (target, trap) = self.proxy_trap(proxy, "getOwnPropertyDescriptor")?;
        match trap {
            Some(trap) => {
                let descriptor = self.call_function(
                    trap,
                    Self::proxy_handler(proxy),
                    vec![target.clone(), key_value(key)],
                )?;
                if !matches!(descriptor, Value::Undefined) && is_primitive(&descriptor) {
                    return Err(self.type_error(
                        "'getOwnPropertyDescriptor' on proxy: trap returned neither object nor undefined",
                    ));
                }
                let target_desc = self.own_property_of(&target, key)?;
                if matches!(descriptor, Value::Undefined) {
                    if let Some(td) = target_desc
                        && (!td.configurable || !self.is_extensible_value(&target)?)
                    {
                        return Err(self.type_error(format!(
                            "'getOwnPropertyDescriptor' on proxy: trap returned undefined for property '{key}' which is non-configurable or non-extensible in the proxy target"
                        )));
                    }
                    return Ok(None);
                }
                let extensible = self.is_extensible_value(&target)?;
                let result_desc = self.descriptor_property(&descriptor)?;
                let all_present = match &result_desc.slot {
                    Slot::Data(_) => [true, true, true, true, false, false],
                    Slot::Accessor(_) => [true, true, false, false, true, true],
                };
                if !descriptor_compatible_with_target(
                    extensible,
                    &result_desc,
                    all_present,
                    target_desc.as_ref(),
                ) {
                    return Err(self.type_error(format!(
                        "'getOwnPropertyDescriptor' on proxy: trap reported incompatibility for property '{key}'"
                    )));
                }
                if !result_desc.configurable {
                    match &target_desc {
                        None => {
                            return Err(self.type_error(format!(
                                "'getOwnPropertyDescriptor' on proxy: trap reported non-configurability for property '{key}' which is non-existent in the proxy target"
                            )));
                        }
                        Some(td) if td.configurable => {
                            return Err(self.type_error(format!(
                                "'getOwnPropertyDescriptor' on proxy: trap reported non-configurability for property '{key}' which is configurable in the proxy target"
                            )));
                        }
                        Some(td)
                            if matches!(result_desc.slot, Slot::Data(_))
                                && !result_desc.writable
                                && td.writable =>
                        {
                            return Err(self.type_error(format!(
                                "'getOwnPropertyDescriptor' on proxy: trap reported non-writability for property '{key}' which is writable in the proxy target"
                            )));
                        }
                        _ => {}
                    }
                }
                Ok(Some(result_desc))
            }
            None => self.own_property_of(&target, key),
        }
    }

    pub(super) fn proxy_define(
        &mut self,
        proxy: &ProxyData,
        key: &str,
        descriptor: &Value,
    ) -> Result<bool, ScriptError> {
        let (target, trap) = self.proxy_trap(proxy, "defineProperty")?;
        let (prop, present) = self.descriptor_from(descriptor)?;
        match trap {
            Some(trap) => {
                let mut entries = Vec::new();
                if present[0] {
                    entries.push(("enumerable".to_string(), Value::Bool(prop.enumerable)));
                }
                if present[1] {
                    entries.push(("configurable".to_string(), Value::Bool(prop.configurable)));
                }
                match &prop.slot {
                    Slot::Data(v) => {
                        if present[2] {
                            entries.push(("value".to_string(), v.clone()));
                        }
                        if present[3] {
                            entries.push(("writable".to_string(), Value::Bool(prop.writable)));
                        }
                    }
                    Slot::Accessor(acc) => {
                        if present[4] {
                            entries.push((
                                "get".to_string(),
                                acc.get.clone().unwrap_or(Value::Undefined),
                            ));
                        }
                        if present[5] {
                            entries.push((
                                "set".to_string(),
                                acc.set.clone().unwrap_or(Value::Undefined),
                            ));
                        }
                    }
                }
                let desc_obj = self.object_from_entries(entries);
                let accepted = self
                    .call_function(
                        trap,
                        Self::proxy_handler(proxy),
                        vec![target.clone(), key_value(key), desc_obj],
                    )?
                    .truthy();
                if !accepted {
                    return Ok(false);
                }
                let target_desc = self.own_property_of(&target, key)?;
                let extensible = self.is_extensible_value(&target)?;
                let setting_config_false = present[1] && !prop.configurable;
                if target_desc.is_none() {
                    if !extensible {
                        return Err(self.type_error(format!(
                            "'defineProperty' on proxy: trap returned truish for defining non-existent property '{key}' on non-extensible target"
                        )));
                    }
                    if setting_config_false {
                        return Err(self.type_error(format!(
                            "'defineProperty' on proxy: trap returned truish for defining non-configurable property '{key}' which is non-existent in target"
                        )));
                    }
                } else if let Some(td) = target_desc {
                    if !descriptor_compatible_with_target(extensible, &prop, present, Some(&td)) {
                        return Err(self.type_error(format!(
                            "'defineProperty' on proxy: trap returned truish for defining incompatible property '{key}'"
                        )));
                    }
                    if setting_config_false && td.configurable {
                        return Err(self.type_error(format!(
                            "'defineProperty' on proxy: trap returned truish for defining non-configurable property '{key}' which is configurable in target"
                        )));
                    }
                    if !td.configurable
                        && matches!(td.slot, Slot::Data(_))
                        && td.writable
                        && present[3]
                        && !prop.writable
                    {
                        return Err(self.type_error(format!(
                            "'defineProperty' on proxy: trap returned truish for defining non-writable property '{key}' which is writable in target"
                        )));
                    }
                }
                Ok(true)
            }
            None => {
                if let Value::Object(object) = &target
                    && let Some(inner) = proxy_of(object)
                {
                    return self.proxy_define(&inner, key, descriptor);
                }
                match self.apply_property_descriptor(&target, key.to_string(), prop, present) {
                    Ok(()) => Ok(true),
                    Err(_) => {
                        self.pending_exception = None;
                        Ok(false)
                    }
                }
            }
        }
    }

    pub(super) fn proxy_prototype(
        &mut self,
        proxy: &ProxyData,
    ) -> Result<Option<ObjectHandle>, ScriptError> {
        let (target, trap) = self.proxy_trap(proxy, "getPrototypeOf")?;
        match trap {
            Some(trap) => {
                let trap_proto = match self.call_function(
                    trap,
                    Self::proxy_handler(proxy),
                    vec![target.clone()],
                )? {
                    Value::Object(prototype) => Some(prototype),
                    Value::Array(array) => Some(self.array_prototype_wrapper(&array)),
                    Value::Null => None,
                    _ => {
                        return Err(self.type_error(
                            "proxy getPrototypeOf trap returned neither object nor null",
                        ));
                    }
                };
                if !self.is_extensible_value(&target)? {
                    let target_proto = self.prototype_of_value(&target)?;
                    let same = match (&trap_proto, &target_proto) {
                        (Some(a), Some(b)) => Self::prototypes_equal(a, b),
                        (None, None) => true,
                        _ => false,
                    };
                    if !same {
                        return Err(self.type_error(
                            "'getPrototypeOf' on proxy: proxy target is non-extensible but the trap did not return its actual prototype",
                        ));
                    }
                }
                Ok(trap_proto)
            }
            None => self.prototype_of_value(&target),
        }
    }

    pub(super) fn call_reflect(
        &mut self,
        method: ReflectMethod,
        arguments: Vec<Value>,
        new_target: Option<Value>,
    ) -> Result<Value, ScriptError> {
        use ReflectMethod as M;
        let first = argument(&arguments, 0);
        let needs_object = !matches!(method, M::ProxyConstructor | M::ProxyRevocable);
        if needs_object && is_primitive(&first) {
            return Err(self.type_error(format!("Reflect.{method:?} called on non-object")));
        }
        match method {
            M::ProxyConstructor | M::ProxyRevocable => {
                if matches!(method, M::ProxyConstructor) && new_target.is_none() {
                    return Err(self.type_error("Constructor Proxy requires 'new'"));
                }
                let target = first;
                let handler = argument(&arguments, 1);
                if is_primitive(&target) || is_primitive(&handler) {
                    return Err(self
                        .type_error("Cannot create proxy with a non-object as target or handler"));
                }
                let proxy = Rc::new(ProxyData {
                    callable: is_callable(&target),
                    constructor: Self::is_constructor(&target),
                    target: RefCell::new(Some(target)),
                    handler: RefCell::new(Some(handler)),
                });
                let object = Value::Object(new_object(JsObject::with_prototype(
                    None,
                    ObjectClass::Proxy(proxy.clone()),
                )));
                if matches!(method, M::ProxyConstructor) {
                    return Ok(object);
                }
                let revoke = self.native_function(Native::ProxyRevoke(proxy), "", 0);
                Ok(self.object_from_entries(vec![
                    ("proxy".to_string(), object),
                    ("revoke".to_string(), revoke),
                ]))
            }
            M::Apply => {
                if !is_callable(&first) {
                    return Err(self.type_error("Reflect.apply target is not a function"));
                }
                let args_list = argument(&arguments, 2);
                if is_primitive(&args_list) {
                    return Err(self.type_error("CreateListFromArrayLike called on non-object"));
                }
                let list = self.array_like_list(&args_list)?;
                self.call_function(first, argument(&arguments, 1), list)
            }
            M::Construct => {
                if !Self::is_constructor(&first) {
                    return Err(self.type_error("Reflect.construct target is not a constructor"));
                }
                let args_list = argument(&arguments, 1);
                if is_primitive(&args_list) {
                    return Err(self.type_error("CreateListFromArrayLike called on non-object"));
                }
                let list = self.array_like_list(&args_list)?;
                let new_target = match argument(&arguments, 2) {
                    Value::Undefined => first.clone(),
                    target if Self::is_constructor(&target) => target,
                    _ => {
                        return Err(
                            self.type_error("Reflect.construct newTarget is not a constructor")
                        );
                    }
                };
                self.construct(first, list, Some(new_target))
            }
            M::DefineProperty => {
                let key = self.coerce_key(argument(&arguments, 1))?;
                let descriptor = argument(&arguments, 2);
                match &first {
                    Value::Object(object) if proxy_of(object).is_some() => {
                        let proxy = proxy_of(object).expect("checked above");
                        Ok(Value::Bool(self.proxy_define(&proxy, &key, &descriptor)?))
                    }
                    _ => {
                        let (property, present) = self.descriptor_from(&descriptor)?;
                        match self.apply_property_descriptor(&first, key, property, present) {
                            Ok(()) => Ok(Value::Bool(true)),
                            Err(_) => {
                                self.pending_exception = None;
                                Ok(Value::Bool(false))
                            }
                        }
                    }
                }
            }
            M::DeleteProperty => {
                let key = self.coerce_key(argument(&arguments, 1))?;
                Ok(Value::Bool(self.delete_property(&first, &key, false)?))
            }
            M::Get => {
                let key = self.coerce_key(argument(&arguments, 1))?;
                let receiver = if arguments.len() > 2 {
                    arguments[2].clone()
                } else {
                    first.clone()
                };
                match &first {
                    Value::Object(object) => self.ordinary_get(object.clone(), &key, &receiver),
                    other => self.get_property(other, &key),
                }
            }
            M::Set => {
                let key = self.coerce_key(argument(&arguments, 1))?;
                let value = argument(&arguments, 2);
                let receiver = if arguments.len() > 3 {
                    arguments[3].clone()
                } else {
                    first.clone()
                };
                if let Value::Object(object) = &first
                    && let Some(proxy) = proxy_of(object)
                {
                    return Ok(Value::Bool(
                        self.proxy_set_bool(&proxy, &key, value, &receiver)?,
                    ));
                }
                let outcome = match &first {
                    Value::Object(object) => {
                        self.ordinary_set(object.clone(), &key, value, &receiver, true)
                    }
                    other => self.set_property(other, &key, value, true),
                };
                match outcome {
                    Ok(()) => Ok(Value::Bool(true)),
                    Err(error) => {
                        let thrown = self.take_thrown(error);
                        let is_type_error = matches!(&thrown, Value::Object(object) if object.borrow().prototype_or(&self.intrinsics).is_some_and(|prototype| ObjectHandle::ptr_eq(&prototype, &self.intrinsics.type_error_prototype)));
                        if is_type_error {
                            Ok(Value::Bool(false))
                        } else {
                            Err(self.throw_value(thrown))
                        }
                    }
                }
            }
            M::GetOwnPropertyDescriptor => {
                let key = self.coerce_key(argument(&arguments, 1))?;
                Ok(match self.own_property_of(&first, &key)? {
                    Some(property) => self.descriptor_value(&property),
                    None => Value::Undefined,
                })
            }
            M::GetPrototypeOf => Ok(Self::prototype_value(self.prototype_of_value(&first)?)),
            M::Has => {
                let key = self.coerce_key(argument(&arguments, 1))?;
                Ok(Value::Bool(self.has_property(&first, &key)?))
            }
            M::IsExtensible => Ok(Value::Bool(self.is_extensible_value(&first)?)),
            M::PreventExtensions => Ok(Value::Bool(self.prevent_extensions_value(&first)?)),
            M::OwnKeys => {
                let keys = self.all_own_property_keys(&first)?;
                Ok(Self::new_array(
                    keys.iter().map(|key| key_value(key)).collect(),
                ))
            }
            M::SetPrototypeOf => {
                let prototype = match argument(&arguments, 1) {
                    Value::Object(prototype) => Some(prototype),
                    Value::Array(array) => Some(self.array_prototype_wrapper(&array)),
                    Value::Null => None,
                    _ => {
                        return Err(
                            self.type_error("Object prototype may only be an Object or null")
                        );
                    }
                };
                if let Value::Array(array) = &first {
                    return Ok(Value::Bool(self.set_array_prototype(array, prototype)));
                }
                let target = self.wrapper_object(&first).map(Value::Object);
                let Value::Object(object) = target.as_ref().unwrap_or(&first) else {
                    return Ok(Value::Bool(false));
                };
                if let Some(proxy) = proxy_of(object) {
                    return Ok(Value::Bool(self.proxy_set_prototype(&proxy, prototype)?));
                }
                match self.set_object_prototype(object, prototype) {
                    Ok(()) => Ok(Value::Bool(true)),
                    Err(_) => {
                        self.pending_exception = None;
                        Ok(Value::Bool(false))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_invariants_reflect_non_constructors_error_stack_and_json() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                const target = {};
                Object.defineProperty(target, "k", { value: 1, writable: false, configurable: false });
                const badGet = new Proxy(target, { get() { return 2; } });
                try { void badGet.k; } catch (e) { seen.push(e.name); }
                const badHas = new Proxy(target, { has() { return false; } });
                try { void ("k" in badHas); } catch (e) { seen.push(e.name); }
                const badKeys = new Proxy(target, { ownKeys() { return []; } });
                try { Object.keys(badKeys); } catch (e) { seen.push(e.name); }
                const badExt = new Proxy(target, { isExtensible() { return false; } });
                try { Object.isExtensible(badExt); } catch (e) { seen.push(e.name); }
                try { new Reflect.get({}, "a"); } catch (e) { seen.push(e.name); }
                const err = new Error("boom");
                seen.push(Object.prototype.hasOwnProperty.call(err, "stack"), typeof err.stack, Error.prototype.stack === undefined);
                err.stack = "custom";
                seen.push(Object.prototype.hasOwnProperty.call(err, "stack"), err.stack);
                seen.push(typeof new Object("hi"), Object.prototype.toString.call(new Object("hi")));
                try { JSON.stringify(Object(1n)); } catch (e) { seen.push(e.name); }
                document.getElementById("out").textContent = seen.join("|");
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some(
                "TypeError|TypeError|TypeError|TypeError|TypeError|false|string|true|true|custom|object|[object String]|TypeError"
            )
        );
    }
}
