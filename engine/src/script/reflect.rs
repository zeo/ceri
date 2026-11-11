// Proxy and Reflect: a proxy forwards each internal operation to a handler trap when the
// handler defines one and to its target otherwise; Reflect exposes the same operations

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, is_symbol_key, new_object};
use super::operations::{is_callable, is_primitive};
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
        Value::Symbol(key.to_string())
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
        let (target, trap) = self.proxy_trap(proxy, "get")?;
        match trap {
            Some(trap) => self.call_function(
                trap,
                Self::proxy_handler(proxy),
                vec![target, key_value(key), receiver.clone()],
            ),
            None => match &target {
                Value::Object(object) => self.ordinary_get(object.clone(), key, receiver),
                other => self.get_property(other, key),
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
        let (target, trap) = self.proxy_trap(proxy, "set")?;
        match trap {
            Some(trap) => {
                let accepted = self
                    .call_function(
                        trap,
                        Self::proxy_handler(proxy),
                        vec![target, key_value(key), value, receiver.clone()],
                    )?
                    .truthy();
                if !accepted && strict {
                    return Err(self.type_error(format!(
                        "'set' on proxy: trap returned falsish for property '{key}'"
                    )));
                }
                Ok(())
            }
            None => match &target {
                Value::Object(object) => {
                    self.ordinary_set(object.clone(), key, value, receiver, strict)
                }
                other => self.set_property(other, key, value, strict),
            },
        }
    }

    pub(super) fn proxy_has(&mut self, proxy: &ProxyData, key: &str) -> Result<bool, ScriptError> {
        let (target, trap) = self.proxy_trap(proxy, "has")?;
        match trap {
            Some(trap) => Ok(self
                .call_function(
                    trap,
                    Self::proxy_handler(proxy),
                    vec![target, key_value(key)],
                )?
                .truthy()),
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
                        vec![target, key_value(key)],
                    )?
                    .truthy();
                if !deleted && strict {
                    return Err(self.type_error(format!(
                        "'deleteProperty' on proxy: trap returned falsish for property '{key}'"
                    )));
                }
                Ok(deleted)
            }
            None => self.delete_property(&target, key, strict),
        }
    }

    pub(super) fn proxy_own_keys(&mut self, proxy: &ProxyData) -> Result<Vec<String>, ScriptError> {
        let (target, trap) = self.proxy_trap(proxy, "ownKeys")?;
        match trap {
            Some(trap) => {
                let list = self.call_function(trap, Self::proxy_handler(proxy), vec![target])?;
                let values = self.array_like_list(&list)?;
                let mut keys = Vec::with_capacity(values.len());
                for value in values {
                    match value {
                        Value::String(text) => keys.push(text.to_string()),
                        Value::Symbol(key) => keys.push(key),
                        _ => return Err(self.type_error("proxy ownKeys trap returned a non-key")),
                    }
                }
                Ok(keys)
            }
            None => {
                let mut keys = self.own_property_keys(&target, false)?;
                keys.extend(self.own_property_keys(&target, true)?);
                Ok(keys)
            }
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
                    vec![target, key_value(key)],
                )?;
                if matches!(descriptor, Value::Undefined) {
                    return Ok(None);
                }
                Ok(Some(self.descriptor_property(&descriptor)?))
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
        match trap {
            Some(trap) => Ok(self
                .call_function(
                    trap,
                    Self::proxy_handler(proxy),
                    vec![target, key_value(key), descriptor.clone()],
                )?
                .truthy()),
            None => {
                self.define_property_value(&target, key.to_string(), descriptor)?;
                Ok(true)
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
                match self.call_function(trap, Self::proxy_handler(proxy), vec![target])? {
                    Value::Object(prototype) => Ok(Some(prototype)),
                    Value::Null => Ok(None),
                    _ => Err(self
                        .type_error("proxy getPrototypeOf trap returned neither object nor null")),
                }
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
                let list = self.array_like_list(&argument(&arguments, 2))?;
                self.call_function(first, argument(&arguments, 1), list)
            }
            M::Construct => {
                if !Self::is_constructor(&first) {
                    return Err(self.type_error("Reflect.construct target is not a constructor"));
                }
                let list = self.array_like_list(&argument(&arguments, 1))?;
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
                    _ => match self.define_property_value(&first, key, &descriptor) {
                        Ok(()) => Ok(Value::Bool(true)),
                        Err(_) => {
                            self.pending_exception = None;
                            Ok(Value::Bool(false))
                        }
                    },
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
                        let is_type_error = matches!(&thrown, Value::Object(object) if object.borrow().prototype_or(&self.intrinsics).is_some_and(|prototype| Rc::ptr_eq(&prototype, &self.intrinsics.type_error_prototype)));
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
            M::GetPrototypeOf => Ok(self
                .prototype_of_value(&first)?
                .map(Value::Object)
                .unwrap_or(Value::Null)),
            M::Has => {
                let key = self.coerce_key(argument(&arguments, 1))?;
                Ok(Value::Bool(self.has_property(&first, &key)?))
            }
            M::IsExtensible => Ok(Value::Bool(match &first {
                Value::Object(object) => match super::reflect::proxy_of(object) {
                    Some(proxy) => {
                        let (target, trap) = self.proxy_trap(&proxy, "isExtensible")?;
                        match trap {
                            Some(trap) => self
                                .call_function(trap, Self::proxy_handler(&proxy), vec![target])?
                                .truthy(),
                            None => {
                                matches!(&target, Value::Object(object) if object.borrow().extensible)
                            }
                        }
                    }
                    None => object.borrow().extensible,
                },
                _ => true,
            })),
            M::PreventExtensions => {
                if let Value::Object(object) = &first {
                    match super::reflect::proxy_of(object) {
                        Some(proxy) => {
                            let (target, trap) = self.proxy_trap(&proxy, "preventExtensions")?;
                            match trap {
                                Some(trap) => {
                                    return Ok(Value::Bool(
                                        self.call_function(
                                            trap,
                                            Self::proxy_handler(&proxy),
                                            vec![target],
                                        )?
                                        .truthy(),
                                    ));
                                }
                                None => {
                                    if let Value::Object(target) = target {
                                        target.borrow_mut().extensible = false;
                                    }
                                }
                            }
                        }
                        None => object.borrow_mut().extensible = false,
                    }
                }
                Ok(Value::Bool(true))
            }
            M::OwnKeys => {
                let mut keys = self.own_property_keys(&first, false)?;
                keys.extend(self.own_property_keys(&first, true)?);
                Ok(Self::new_array(
                    keys.iter().map(|key| key_value(key)).collect(),
                ))
            }
            M::SetPrototypeOf => {
                let prototype = match argument(&arguments, 1) {
                    Value::Object(prototype) => Some(prototype),
                    Value::Null => None,
                    _ => {
                        return Err(
                            self.type_error("Object prototype may only be an Object or null")
                        );
                    }
                };
                let target = self.wrapper_object(&first).map(Value::Object);
                let Value::Object(object) = target.as_ref().unwrap_or(&first) else {
                    return Ok(Value::Bool(false));
                };
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
