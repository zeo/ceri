// ordinary objects: insertion-ordered properties with attributes and accessors, a prototype
// link and an internal class slot for functions, boxed primitives and other exotic kinds

use super::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

pub(super) type ObjectHandle = Rc<RefCell<JsObject>>;

#[derive(Clone)]
pub(super) enum Slot {
    Data(Value),
    Accessor {
        get: Option<Value>,
        set: Option<Value>,
    },
}

#[derive(Clone)]
pub(super) struct Property {
    pub(super) slot: Slot,
    pub(super) enumerable: bool,
    pub(super) writable: bool,
    pub(super) configurable: bool,
}

impl Property {
    pub(super) fn data(value: Value) -> Self {
        Self {
            slot: Slot::Data(value),
            enumerable: true,
            writable: true,
            configurable: true,
        }
    }

    // builtin methods and class members are writable and configurable but not enumerable
    pub(super) fn hidden(value: Value) -> Self {
        Self {
            enumerable: false,
            ..Self::data(value)
        }
    }

    pub(super) fn value(&self) -> Option<&Value> {
        match &self.slot {
            Slot::Data(value) => Some(value),
            Slot::Accessor { .. } => None,
        }
    }
}

pub(super) struct ArrayIteration {
    pub(super) target: Value,
    pub(super) index: usize,
    pub(super) kind: ArrayIterationKind,
    pub(super) done: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ArrayIterationKind {
    Values,
    Keys,
    Entries,
}

// the internal kind of an object, which decides how it is called, printed and compared
#[derive(Clone, Default)]
pub(super) enum ObjectClass {
    #[default]
    Ordinary,
    Function(Rc<super::vm::Closure>),
    Bound {
        target: Value,
        this: Value,
        arguments: Vec<Value>,
    },
    Arguments,
    Boolean(bool),
    Number(f64),
    String(String),
    Symbol(String),
    BigInt(Rc<super::bigint::BigInt>),
    Error,
    Generator(Rc<RefCell<super::vm::GeneratorState>>),
    AsyncGenerator(Rc<RefCell<super::vm::AsyncGeneratorData>>),
    // a function implemented by the engine, such as Object.keys
    Native(super::realm::Native),
    RegExp(Rc<super::pattern::RegExpData>),
    Proxy(Rc<super::reflect::ProxyData>),
    Date(f64),
    Map(Rc<RefCell<super::collections::Collection>>),
    Set(Rc<RefCell<super::collections::Collection>>),
    WeakMap(Rc<RefCell<super::collections::Collection>>),
    WeakSet(Rc<RefCell<super::collections::Collection>>),
    WeakRef(std::rc::Weak<RefCell<JsObject>>),
    // an Array Iterator, which reads its target live on every step
    ArrayIterator(Rc<RefCell<ArrayIteration>>),
    // an object that holds event listeners, keyed by this id in the runtime's registry
    EventTarget(u32),
    // a Mutation, Resize or IntersectionObserver, keyed in the runtime's registry
    Observer(u32),
    // a live Map or Set iterator
    Cursor(Rc<RefCell<super::collections::Cursor>>),
    DataView {
        buffer: super::BytesHandle,
        start: usize,
        end: usize,
    },
    TextDecoder(Rc<RefCell<super::encoding::DecoderState>>),
    Intl(Rc<super::intl::IntlState>),
    Url(Rc<RefCell<super::urls::UrlState>>),
    UrlSearchParams(Rc<RefCell<super::urls::ParamsState>>),
    // a TreeWalker or NodeIterator
    Walk(Rc<RefCell<super::traversal::WalkState>>),
    // what an iterator helper such as map or take hands back
    IteratorHelper(Rc<RefCell<super::iterator_helpers::HelperState>>),
}

#[derive(Clone)]
pub(super) struct JsObject {
    entries: Vec<(String, Property)>,
    // built once an object grows past a handful of properties
    index: Option<HashMap<String, usize>>,
    prototype: Option<ObjectHandle>,
    // objects built by host code without a realm at hand inherit from Object.prototype
    orphan: bool,
    pub(super) extensible: bool,
    pub(super) class: ObjectClass,
}

const INDEX_THRESHOLD: usize = 12;

impl Default for JsObject {
    fn default() -> Self {
        Self::new()
    }
}

impl FromIterator<(String, Value)> for JsObject {
    fn from_iter<I: IntoIterator<Item = (String, Value)>>(iterator: I) -> Self {
        let mut object = Self::new();
        for (key, value) in iterator {
            object.insert(key, value);
        }
        object
    }
}

// an integer key orders before string keys, as own property enumeration requires
pub(super) fn array_index(key: &str) -> Option<u32> {
    if key == "0" {
        return Some(0);
    }
    if key.starts_with('0') || key.is_empty() || key.len() > 10 {
        return None;
    }
    key.parse::<u32>().ok().filter(|index| *index != u32::MAX)
}

impl JsObject {
    pub(super) fn new() -> Self {
        Self {
            entries: Vec::new(),
            index: None,
            prototype: None,
            orphan: true,
            extensible: true,
            class: ObjectClass::Ordinary,
        }
    }

    pub(super) fn with_prototype(prototype: Option<ObjectHandle>, class: ObjectClass) -> Self {
        Self {
            prototype,
            orphan: false,
            class,
            ..Self::new()
        }
    }

    // the prototype, defaulting host-built objects to the intrinsic their class implies
    pub(super) fn prototype_or(
        &self,
        intrinsics: &super::realm::Intrinsics,
    ) -> Option<ObjectHandle> {
        match &self.prototype {
            Some(prototype) => Some(prototype.clone()),
            None if self.orphan => Some(match &self.class {
                ObjectClass::Map(_) => intrinsics.map_prototype.clone(),
                ObjectClass::Set(_) => intrinsics.set_prototype.clone(),
                ObjectClass::Date(_) => intrinsics.date_prototype.clone(),
                _ => intrinsics.object_prototype.clone(),
            }),
            None => None,
        }
    }

    // a host-built object of a class, whose prototype comes from the realm that reads it
    pub(super) fn with_class(class: ObjectClass) -> Self {
        Self {
            class,
            ..Self::new()
        }
    }

    // the prototype link as stored, without the Object.prototype default of host objects
    pub(super) fn explicit_prototype(&self) -> Option<ObjectHandle> {
        self.prototype.clone()
    }

    pub(super) fn set_prototype(&mut self, prototype: Option<ObjectHandle>) {
        self.prototype = prototype;
        self.orphan = false;
    }

    fn position(&self, key: &str) -> Option<usize> {
        match &self.index {
            Some(index) => index.get(key).copied(),
            None => self.entries.iter().position(|(name, _)| name == key),
        }
    }

    fn rebuild_index(&mut self) {
        if self.entries.len() > INDEX_THRESHOLD {
            self.index = Some(
                self.entries
                    .iter()
                    .enumerate()
                    .map(|(position, (name, _))| (name.clone(), position))
                    .collect(),
            );
        } else {
            self.index = None;
        }
    }

    pub(super) fn property(&self, key: &str) -> Option<&Property> {
        self.position(key).map(|position| &self.entries[position].1)
    }

    pub(super) fn property_mut(&mut self, key: &str) -> Option<&mut Property> {
        self.position(key)
            .map(move |position| &mut self.entries[position].1)
    }

    // defines or replaces a property with exact attributes
    pub(super) fn define(&mut self, key: String, property: Property) {
        if let Some(position) = self.position(&key) {
            self.entries[position].1 = property;
            return;
        }
        if let Some(index) = &mut self.index {
            index.insert(key.clone(), self.entries.len());
        }
        self.entries.push((key, property));
        if self.index.is_none() && self.entries.len() > INDEX_THRESHOLD {
            self.rebuild_index();
        }
    }

    // the data value of an own property; accessors read as absent for host code
    pub(super) fn get(&self, key: &str) -> Option<&Value> {
        self.property(key).and_then(Property::value)
    }

    pub(super) fn contains_key(&self, key: &str) -> bool {
        self.position(key).is_some()
    }

    // sets a data value, keeping the attributes of an existing property
    pub(super) fn insert(&mut self, key: String, value: Value) -> Option<Value> {
        if let Some(property) = self.property_mut(&key) {
            let previous = property.value().cloned();
            property.slot = Slot::Data(value);
            return previous;
        }
        self.define(key, Property::data(value));
        None
    }

    pub(super) fn remove(&mut self, key: &str) -> Option<Value> {
        let position = self.position(key)?;
        let (_, property) = self.entries.remove(position);
        if self.index.is_some() {
            self.rebuild_index();
        }
        property.value().cloned()
    }

    // own property keys in spec order: integer keys ascending, then strings in insertion order
    pub(super) fn ordered_keys(&self) -> Vec<String> {
        let mut integers = Vec::new();
        let mut strings = Vec::new();
        for (key, _) in &self.entries {
            match array_index(key) {
                Some(index) => integers.push((index, key.clone())),
                None => strings.push(key.clone()),
            }
        }
        integers.sort_unstable_by_key(|(index, _)| *index);
        integers
            .into_iter()
            .map(|(_, key)| key)
            .chain(strings)
            .collect()
    }

    // enumerable own data properties in spec order, which most host code reads
    pub(super) fn iter(&self) -> impl Iterator<Item = (String, Value)> + '_ {
        self.ordered_keys().into_iter().filter_map(move |key| {
            let property = self.property(&key)?;
            if !property.enumerable || is_symbol_key(&key) {
                return None;
            }
            let value = property.value()?.clone();
            Some((key, value))
        })
    }

    pub(super) fn keys(&self) -> impl Iterator<Item = String> + '_ {
        self.iter().map(|(key, _)| key)
    }

    pub(super) fn entries(&self) -> &[(String, Property)] {
        &self.entries
    }

    pub(super) fn freeze(&mut self, writable: bool) {
        self.extensible = false;
        for (_, property) in &mut self.entries {
            property.configurable = false;
            if !writable && matches!(property.slot, Slot::Data(_)) {
                property.writable = false;
            }
        }
    }

    pub(super) fn is_callable(&self) -> bool {
        match &self.class {
            ObjectClass::Function(_) | ObjectClass::Bound { .. } | ObjectClass::Native(_) => true,
            ObjectClass::Proxy(proxy) => proxy.callable,
            _ => false,
        }
    }
}

// symbol-keyed properties share the string key space under a prefix no source string uses
pub(super) const SYMBOL_PREFIX: &str = "\u{10ffff}@@";

pub(super) fn is_symbol_key(key: &str) -> bool {
    key.starts_with(SYMBOL_PREFIX) || key.starts_with("@@")
}

// private names and engine-internal slots share another prefix and are never enumerated
pub(super) const PRIVATE_PREFIX: &str = "\u{10ffff}#";

pub(super) fn is_private_key(key: &str) -> bool {
    key.starts_with(PRIVATE_PREFIX)
}

pub(super) fn new_object(object: JsObject) -> ObjectHandle {
    Rc::new(RefCell::new(object))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_keep_insertion_order_with_integer_keys_first() {
        let mut object = JsObject::new();
        for key in ["b", "2", "a", "1", "10", "01"] {
            object.insert(key.to_string(), Value::Number(0.0));
        }
        assert_eq!(object.ordered_keys(), ["1", "2", "10", "b", "a", "01"]);
        object.remove("a");
        assert_eq!(
            object.keys().collect::<Vec<_>>(),
            ["1", "2", "10", "b", "01"]
        );
    }

    #[test]
    fn large_objects_switch_to_an_index_and_stay_consistent() {
        let mut object = JsObject::new();
        for index in 0..40 {
            object.insert(format!("key{index}"), Value::Number(index as f64));
        }
        object.remove("key3");
        assert!(object.get("key3").is_none());
        assert!(matches!(object.get("key39"), Some(Value::Number(value)) if *value == 39.0));
        object.define("hidden".to_string(), Property::hidden(Value::Null));
        assert!(object.contains_key("hidden"));
        assert_eq!(object.keys().count(), 39);
    }
}
