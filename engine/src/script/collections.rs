// Map, Set, WeakMap, WeakSet and WeakRef: insertion-ordered tables with a hash index keyed by
// SameValueZero identity, tombstones so live iterators see later additions and skip
// deletions, and weak keys that are pruned once their object is gone

use super::object::{JsObject, ObjectClass, ObjectHandle, new_object};
use super::operations::{is_callable, is_primitive};
use super::*;
use std::collections::HashMap;
use std::rc::Weak;

// the identity a key hashes by: SameValueZero for primitives, the allocation for objects,
// and the node for attached elements, whose handles are recreated on each access
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(super) enum KeyId {
    Undefined,
    Null,
    Bool(bool),
    Number(u64),
    String(JsStr),
    Symbol(String),
    BigInt(Rc<super::bigint::BigInt>),
    Node(usize, u8),
    Address(usize),
}

pub(super) fn key_id(value: &Value) -> KeyId {
    match value {
        Value::Undefined | Value::Uninitialized => KeyId::Undefined,
        Value::Null => KeyId::Null,
        Value::Bool(flag) => KeyId::Bool(*flag),
        Value::Number(number) => {
            let normalized = if *number == 0.0 {
                0.0
            } else if number.is_nan() {
                f64::NAN
            } else {
                *number
            };
            KeyId::Number(normalized.to_bits())
        }
        Value::String(text) => KeyId::String(text.clone()),
        Value::Symbol(key) => KeyId::Symbol(key.to_string()),
        Value::BigInt(value) => KeyId::BigInt(value.clone()),
        Value::Binding(cell) | Value::ConstBinding(cell) => key_id(&cell.borrow()),
        // a node and its style, dataset and class list views share one handle
        Value::Element(handle) => KeyId::Node(Rc::as_ptr(handle) as usize, 0),
        Value::Style(handle) => KeyId::Node(Rc::as_ptr(handle) as usize, 1),
        Value::Dataset(handle) => KeyId::Node(Rc::as_ptr(handle) as usize, 2),
        Value::ClassList(handle) => KeyId::Node(Rc::as_ptr(handle) as usize, 3),
        Value::AnimatedClass(handle) => KeyId::Node(Rc::as_ptr(handle) as usize, 4),
        other => KeyId::Address(value_address(other)),
    }
}

// a stable address for any reference value
pub(super) fn value_address(value: &Value) -> usize {
    macro_rules! address {
        ($handle:expr) => {
            Rc::as_ptr($handle) as *const u8 as usize
        };
    }
    match value {
        Value::Object(handle) => address!(handle),
        Value::Array(handle) => address!(handle),
        Value::Promise(handle) => address!(handle),
        Value::Iterator(handle) => address!(handle),
        Value::Error(handle) => address!(handle),
        Value::Request(handle) => address!(handle),
        Value::Response(handle) => address!(handle),
        Value::Headers(handle) => address!(handle),
        Value::Blob(handle) => address!(handle),
        Value::File(handle) => address!(handle),
        Value::FormData(handle) => address!(handle),
        Value::Bytes(handle) => address!(handle),
        Value::TypedArray(handle) => address!(handle),
        Value::Worker(handle) => address!(handle),
        Value::AbortController(handle) => address!(handle),
        Value::AbortSignal(handle) => address!(handle),
        Value::Range(handle) => address!(handle),
        Value::Selection(handle) => address!(handle),
        Value::Event(handle) => address!(handle),
        Value::NodeList(handle) => address!(handle),
        Value::VmIterator(handle) => address!(handle),
        // singletons such as document and window compare equal to themselves
        other => {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::mem::discriminant(other).hash(&mut hasher);
            hasher.finish() as usize
        }
    }
}

#[derive(Default)]
pub(super) struct Collection {
    entries: Vec<Option<(Value, Value)>>,
    index: HashMap<KeyId, usize>,
    live: usize,
    // weak tables hold their object keys through these, so an unreachable key frees its entry
    weak_keys: Vec<Option<Weak<RefCell<JsObject>>>>,
    weak: bool,
    // iterations in progress hold positions, so the table only compacts when there are none
    cursors: usize,
}

impl Collection {
    pub(super) fn new(weak: bool) -> Self {
        Collection {
            weak,
            ..Collection::default()
        }
    }

    pub(super) fn len(&self) -> usize {
        self.live
    }

    pub(super) fn get(&self, key: &Value) -> Option<Value> {
        let position = *self.index.get(&key_id(key))?;
        self.entries[position]
            .as_ref()
            .map(|(_, value)| value.clone())
    }

    pub(super) fn has(&self, key: &Value) -> bool {
        self.index.contains_key(&key_id(key))
    }

    pub(super) fn set(&mut self, key: Value, value: Value) {
        let id = key_id(&key);
        if let Some(position) = self.index.get(&id) {
            if let Some((_, slot)) = &mut self.entries[*position] {
                *slot = value;
            }
            return;
        }
        // -0 is stored as +0
        let key = match key {
            Value::Number(number) if number == 0.0 && number.is_sign_negative() => {
                Value::Number(0.0)
            }
            key => key,
        };
        if self.weak {
            self.prune();
            let weak = match &key {
                Value::Object(object) => Some(Rc::downgrade(object)),
                _ => None,
            };
            self.weak_keys.push(weak);
            // the strong key is dropped; the weak reference and identity remain
            let stored_key = if matches!(key, Value::Object(_)) {
                Value::Undefined
            } else {
                key
            };
            self.entries.push(Some((stored_key, value)));
        } else {
            self.entries.push(Some((key, value)));
        }
        self.index.insert(id, self.entries.len() - 1);
        self.live += 1;
    }

    pub(super) fn delete(&mut self, key: &Value) -> bool {
        let Some(position) = self.index.remove(&key_id(key)) else {
            return false;
        };
        self.entries[position] = None;
        if self.weak {
            self.weak_keys[position] = None;
        }
        self.live -= 1;
        self.compact_if_sparse();
        true
    }

    pub(super) fn clear(&mut self) {
        for entry in &mut self.entries {
            *entry = None;
        }
        self.index.clear();
        self.weak_keys.iter_mut().for_each(|key| *key = None);
        self.live = 0;
    }

    // the entry at a cursor position, skipping tombstones; None past the end
    pub(super) fn entry_at(&self, position: usize) -> Option<(usize, Value, Value)> {
        (position..self.entries.len()).find_map(|index| {
            self.entries[index]
                .as_ref()
                .map(|(key, value)| (index, key.clone(), value.clone()))
        })
    }

    pub(super) fn values(&self) -> Vec<(Value, Value)> {
        self.entries.iter().flatten().cloned().collect()
    }

    // drops weak entries whose key object has been freed
    fn prune(&mut self) {
        if !self.weak {
            return;
        }
        let dead: Vec<usize> = self
            .weak_keys
            .iter()
            .enumerate()
            .filter(|(_, key)| key.as_ref().is_some_and(|key| key.strong_count() == 0))
            .map(|(position, _)| position)
            .collect();
        if dead.is_empty() {
            return;
        }
        self.index.retain(|_, position| !dead.contains(position));
        for position in dead {
            self.entries[position] = None;
            self.weak_keys[position] = None;
            self.live -= 1;
        }
    }

    // rebuilds the table once tombstones outnumber live entries and nothing is iterating
    fn compact_if_sparse(&mut self) {
        if self.cursors > 0 || self.entries.len() < 64 || self.live * 2 > self.entries.len() {
            return;
        }
        let mut entries = Vec::with_capacity(self.live);
        let mut weak_keys = Vec::new();
        for (position, entry) in std::mem::take(&mut self.entries).into_iter().enumerate() {
            if let Some(entry) = entry {
                entries.push(Some(entry));
                if self.weak {
                    weak_keys.push(self.weak_keys[position].take());
                }
            }
        }
        self.entries = entries;
        self.weak_keys = weak_keys;
        self.index = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(position, entry)| {
                let (key, _) = entry.as_ref()?;
                let id = match self
                    .weak_keys
                    .get(position)
                    .cloned()
                    .flatten()
                    .and_then(|key| key.upgrade())
                {
                    Some(object) => key_id(&Value::Object(object)),
                    None => key_id(key),
                };
                Some((id, position))
            })
            .collect();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CollectionKind {
    Map,
    Set,
    WeakMap,
    WeakSet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IterationKind {
    Keys,
    Values,
    Entries,
}

pub(super) struct Cursor {
    pub(super) collection: Rc<RefCell<Collection>>,
    pub(super) position: usize,
    pub(super) kind: IterationKind,
    pub(super) set: bool,
    pub(super) done: bool,
}

impl Cursor {
    fn new(collection: Rc<RefCell<Collection>>, kind: IterationKind, set: bool) -> Self {
        collection.borrow_mut().cursors += 1;
        Cursor {
            collection,
            position: 0,
            kind,
            set,
            done: false,
        }
    }

    fn finish(&mut self) {
        if !self.done {
            self.done = true;
            if let Ok(mut collection) = self.collection.try_borrow_mut() {
                collection.cursors = collection.cursors.saturating_sub(1);
            }
        }
    }
}

impl Drop for Cursor {
    fn drop(&mut self) {
        self.finish();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CollectionMethod {
    Constructor(CollectionKind),
    Get,
    Set,
    Add,
    Has,
    Delete,
    Clear,
    Size,
    ForEach,
    Iterate(IterationKind),
    CursorNext,
    GroupBy(bool),
    Union,
    Intersection,
    Difference,
    SymmetricDifference,
    IsSubsetOf,
    IsSupersetOf,
    IsDisjointFrom,
    WeakRefConstructor,
    WeakRefDeref,
    RegistryConstructor,
    Register,
    Unregister,
}

// a FinalizationRegistry: the callback, and the cells whose targets it watches
pub(super) struct Registry {
    cleanup: Value,
    cells: Vec<RegistryCell>,
}

struct RegistryCell {
    target: Weak<RefCell<JsObject>>,
    held: Value,
    token: Option<Weak<RefCell<JsObject>>>,
}

fn argument(arguments: &[Value], index: usize) -> Value {
    arguments.get(index).cloned().unwrap_or(Value::Undefined)
}

impl ScriptRuntime {
    fn collection_of(
        &mut self,
        this: &Value,
        kind: CollectionKind,
        method: &str,
    ) -> Result<Rc<RefCell<Collection>>, ScriptError> {
        if let Value::Object(object) = this {
            let class = object.borrow().class.clone();
            let found = match (class, kind) {
                (ObjectClass::Map(collection), CollectionKind::Map)
                | (ObjectClass::Set(collection), CollectionKind::Set)
                | (ObjectClass::WeakMap(collection), CollectionKind::WeakMap)
                | (ObjectClass::WeakSet(collection), CollectionKind::WeakSet) => Some(collection),
                _ => None,
            };
            if let Some(collection) = found {
                return Ok(collection);
            }
        }
        Err(self.type_error(format!(
            "Method {kind:?}.prototype.{method} called on incompatible receiver"
        )))
    }

    fn collection_kind(this: &Value) -> Option<CollectionKind> {
        let Value::Object(object) = this else {
            return None;
        };
        match object.borrow().class {
            ObjectClass::Map(_) => Some(CollectionKind::Map),
            ObjectClass::Set(_) => Some(CollectionKind::Set),
            ObjectClass::WeakMap(_) => Some(CollectionKind::WeakMap),
            ObjectClass::WeakSet(_) => Some(CollectionKind::WeakSet),
            _ => None,
        }
    }

    fn check_weak_key(&mut self, key: &Value) -> Result<(), ScriptError> {
        if is_primitive(key) && !matches!(key, Value::Symbol(_)) {
            return Err(self.type_error("Invalid value used as weak collection key"));
        }
        Ok(())
    }

    pub(super) fn new_collection(
        &self,
        kind: CollectionKind,
        prototype: Option<ObjectHandle>,
    ) -> (Value, Rc<RefCell<Collection>>) {
        let weak = matches!(kind, CollectionKind::WeakMap | CollectionKind::WeakSet);
        let collection = Rc::new(RefCell::new(Collection::new(weak)));
        let (class, fallback) = match kind {
            CollectionKind::Map => (
                ObjectClass::Map(collection.clone()),
                &self.intrinsics.map_prototype,
            ),
            CollectionKind::Set => (
                ObjectClass::Set(collection.clone()),
                &self.intrinsics.set_prototype,
            ),
            CollectionKind::WeakMap => (
                ObjectClass::WeakMap(collection.clone()),
                &self.intrinsics.weak_map_prototype,
            ),
            CollectionKind::WeakSet => (
                ObjectClass::WeakSet(collection.clone()),
                &self.intrinsics.weak_set_prototype,
            ),
        };
        let prototype = prototype.unwrap_or_else(|| fallback.clone());
        let object = new_object(JsObject::with_prototype(Some(prototype), class));
        (Value::Object(object), collection)
    }

    fn cursor_value(&self, cursor: Cursor) -> Value {
        let prototype = self.intrinsics.collection_iterator_prototype.clone();
        Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Cursor(Rc::new(RefCell::new(cursor))),
        )))
    }

    // the next item of a live map or set iterator
    pub(super) fn cursor_step(cursor: &RefCell<Cursor>) -> Option<Value> {
        let mut cursor = cursor.borrow_mut();
        if cursor.done {
            return None;
        }
        let found = cursor.collection.borrow().entry_at(cursor.position);
        match found {
            None => {
                cursor.finish();
                None
            }
            Some((position, key, value)) => {
                cursor.position = position + 1;
                Some(match (cursor.kind, cursor.set) {
                    (IterationKind::Keys, _) => key,
                    (IterationKind::Values, true) => key,
                    (IterationKind::Values, false) => value,
                    (IterationKind::Entries, true) => Self::new_array(vec![key.clone(), key]),
                    (IterationKind::Entries, false) => Self::new_array(vec![key, value]),
                })
            }
        }
    }

    // a set-like argument for the set algebra methods: its size, has and keys
    fn set_like_values(&mut self, other: &Value) -> Result<Vec<Value>, ScriptError> {
        if let Value::Object(object) = other {
            let class = object.borrow().class.clone();
            if let ObjectClass::Set(collection) | ObjectClass::Map(collection) = class {
                return Ok(collection
                    .borrow()
                    .values()
                    .into_iter()
                    .map(|(key, _)| key)
                    .collect());
            }
        }
        let keys = self.get_property(other, "keys")?;
        if !is_callable(&keys) {
            return Err(self.type_error("The argument must be set-like"));
        }
        let iterator = self.call_function(keys, other.clone(), Vec::new())?;
        let next = self.get_property(&iterator, "next")?;
        let mut values = Vec::new();
        loop {
            let result = self.call_function(next.clone(), iterator.clone(), Vec::new())?;
            if self.get_property(&result, "done")?.truthy() {
                break;
            }
            values.push(self.get_property(&result, "value")?);
        }
        Ok(values)
    }

    pub(super) fn call_collection(
        &mut self,
        method: CollectionMethod,
        this: Value,
        arguments: Vec<Value>,
        new_target: Option<Value>,
    ) -> Result<Value, ScriptError> {
        use CollectionMethod as M;
        let first = argument(&arguments, 0);
        match method {
            M::Constructor(kind) => {
                let Some(new_target) = new_target else {
                    return Err(self.type_error(format!("Constructor {kind:?} requires 'new'")));
                };
                let prototype = match &new_target {
                    Value::Object(_) => Some(self.prototype_from_constructor(&new_target)?),
                    _ => None,
                };
                let (object, _) = self.new_collection(kind, prototype);
                if matches!(first, Value::Undefined | Value::Null) {
                    return Ok(object);
                }
                let adder = self.get_property(
                    &object,
                    if matches!(kind, CollectionKind::Map | CollectionKind::WeakMap) {
                        "set"
                    } else {
                        "add"
                    },
                )?;
                if !is_callable(&adder) {
                    return Err(self.type_error("collection adder is not a function"));
                }
                let entries = matches!(kind, CollectionKind::Map | CollectionKind::WeakMap);
                self.for_each_iterated(first, |runtime, item| {
                    let arguments = if entries {
                        if is_primitive(&item) {
                            return Err(runtime.type_error("Iterator value is not an entry object"));
                        }
                        let key = runtime.get_index(&item, Value::Number(0.0))?;
                        let value = runtime.get_index(&item, Value::Number(1.0))?;
                        vec![key, value]
                    } else {
                        vec![item]
                    };
                    runtime
                        .call_function(adder.clone(), object.clone(), arguments)
                        .map(|_| ())
                })?;
                Ok(object)
            }
            M::Get
            | M::Set
            | M::Add
            | M::Has
            | M::Delete
            | M::Clear
            | M::Size
            | M::ForEach
            | M::Iterate(_) => {
                let Some(kind) = Self::collection_kind(&this) else {
                    return Err(self.type_error(format!(
                        "collection method {method:?} called on incompatible receiver"
                    )));
                };
                let collection = self.collection_of(&this, kind, &format!("{method:?}"))?;
                let weak = matches!(kind, CollectionKind::WeakMap | CollectionKind::WeakSet);
                match method {
                    M::Get => Ok(collection.borrow().get(&first).unwrap_or(Value::Undefined)),
                    M::Has => Ok(Value::Bool(collection.borrow().has(&first))),
                    M::Set | M::Add => {
                        if weak {
                            self.check_weak_key(&first)?;
                        }
                        let value = if matches!(method, M::Set) {
                            argument(&arguments, 1)
                        } else {
                            Value::Undefined
                        };
                        if collection.borrow().len() >= MAX_RUNTIME_ARRAY_LENGTH {
                            return Err(
                                self.range_error("collection exceeds the deterministic size limit")
                            );
                        }
                        collection.borrow_mut().set(first, value);
                        Ok(this)
                    }
                    M::Delete => Ok(Value::Bool(collection.borrow_mut().delete(&first))),
                    M::Clear => {
                        collection.borrow_mut().clear();
                        Ok(Value::Undefined)
                    }
                    M::Size => Ok(Value::Number(collection.borrow().len() as f64)),
                    M::ForEach => {
                        if !is_callable(&first) {
                            return Err(self.type_error("forEach callback is not a function"));
                        }
                        let receiver = argument(&arguments, 1);
                        let mut cursor = Cursor::new(collection, IterationKind::Entries, false);
                        let set = kind == CollectionKind::Set;
                        loop {
                            let found = cursor.collection.borrow().entry_at(cursor.position);
                            let Some((index, key, value)) = found else {
                                break;
                            };
                            cursor.position = index + 1;
                            let value = if set { key.clone() } else { value };
                            self.call_function(
                                first.clone(),
                                receiver.clone(),
                                vec![value, key, this.clone()],
                            )?;
                        }
                        Ok(Value::Undefined)
                    }
                    M::Iterate(iteration) => Ok(self.cursor_value(Cursor::new(
                        collection,
                        iteration,
                        kind == CollectionKind::Set,
                    ))),
                    _ => unreachable!("handled above"),
                }
            }
            M::CursorNext => {
                let cursor = match &this {
                    Value::Object(object) => match &object.borrow().class {
                        ObjectClass::Cursor(cursor) => Some(cursor.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(cursor) = cursor else {
                    return Err(self.type_error("next method called on incompatible receiver"));
                };
                Ok(match Self::cursor_step(&cursor) {
                    Some(value) => self.iterator_result(value, false),
                    None => self.iterator_result(Value::Undefined, true),
                })
            }
            M::GroupBy(into_map) => {
                let callback = argument(&arguments, 1);
                if !is_callable(&callback) {
                    return Err(self.type_error("groupBy callback is not a function"));
                }
                let items = self.iterate_to_vec(first)?;
                let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
                let mut index_of: HashMap<KeyId, usize> = HashMap::new();
                for (index, item) in items.into_iter().enumerate() {
                    let key = self.call_function(
                        callback.clone(),
                        Value::Undefined,
                        vec![item.clone(), Value::Number(index as f64)],
                    )?;
                    let key = if into_map {
                        key
                    } else {
                        Value::String(self.coerce_key(key)?.into())
                    };
                    let id = key_id(&key);
                    match index_of.get(&id) {
                        Some(position) => groups[*position].1.push(item),
                        None => {
                            index_of.insert(id, groups.len());
                            groups.push((key, vec![item]));
                        }
                    }
                }
                if into_map {
                    let (map, collection) = self.new_collection(CollectionKind::Map, None);
                    for (key, items) in groups {
                        collection.borrow_mut().set(key, Self::new_array(items));
                    }
                    Ok(map)
                } else {
                    let object = new_object(JsObject::with_prototype(None, ObjectClass::Ordinary));
                    for (key, items) in groups {
                        object
                            .borrow_mut()
                            .insert(key.string_value(), Self::new_array(items));
                    }
                    Ok(Value::Object(object))
                }
            }
            M::Union
            | M::Intersection
            | M::Difference
            | M::SymmetricDifference
            | M::IsSubsetOf
            | M::IsSupersetOf
            | M::IsDisjointFrom => {
                let collection =
                    self.collection_of(&this, CollectionKind::Set, &format!("{method:?}"))?;
                let others = self.set_like_values(&first)?;
                let mine: Vec<Value> = collection
                    .borrow()
                    .values()
                    .into_iter()
                    .map(|(key, _)| key)
                    .collect();
                let other_ids: std::collections::HashSet<KeyId> =
                    others.iter().map(key_id).collect();
                let my_ids: std::collections::HashSet<KeyId> = mine.iter().map(key_id).collect();
                let result: Vec<Value> = match method {
                    M::Union => mine
                        .iter()
                        .cloned()
                        .chain(
                            others
                                .iter()
                                .filter(|value| !my_ids.contains(&key_id(value)))
                                .cloned(),
                        )
                        .collect(),
                    M::Intersection => mine
                        .iter()
                        .filter(|value| other_ids.contains(&key_id(value)))
                        .cloned()
                        .collect(),
                    M::Difference => mine
                        .iter()
                        .filter(|value| !other_ids.contains(&key_id(value)))
                        .cloned()
                        .collect(),
                    M::SymmetricDifference => mine
                        .iter()
                        .filter(|value| !other_ids.contains(&key_id(value)))
                        .chain(
                            others
                                .iter()
                                .filter(|value| !my_ids.contains(&key_id(value))),
                        )
                        .cloned()
                        .collect(),
                    M::IsSubsetOf => return Ok(Value::Bool(my_ids.is_subset(&other_ids))),
                    M::IsSupersetOf => return Ok(Value::Bool(other_ids.is_subset(&my_ids))),
                    _ => return Ok(Value::Bool(my_ids.is_disjoint(&other_ids))),
                };
                let (set, table) = self.new_collection(CollectionKind::Set, None);
                for value in result {
                    table.borrow_mut().set(value, Value::Undefined);
                }
                Ok(set)
            }
            M::WeakRefConstructor => {
                if new_target.is_none() {
                    return Err(self.type_error("Constructor WeakRef requires 'new'"));
                }
                let Value::Object(target) = &first else {
                    return Err(self.type_error("WeakRef: target must be an object"));
                };
                let prototype = self.intrinsics.weak_ref_prototype.clone();
                Ok(Value::Object(new_object(JsObject::with_prototype(
                    Some(prototype),
                    ObjectClass::WeakRef(Rc::downgrade(target)),
                ))))
            }
            M::RegistryConstructor => {
                if new_target.is_none() {
                    return Err(self.type_error("Constructor FinalizationRegistry requires 'new'"));
                }
                if !is_callable(&first) {
                    return Err(self.type_error("FinalizationRegistry: cleanup must be callable"));
                }
                let registry = Rc::new(RefCell::new(Registry {
                    cleanup: first,
                    cells: Vec::new(),
                }));
                self.registries.push(Rc::downgrade(&registry));
                let prototype = match &new_target {
                    Some(target @ Value::Object(_)) => self.prototype_from_constructor(target)?,
                    _ => self.intrinsics.registry_prototype.clone(),
                };
                Ok(Value::Object(new_object(JsObject::with_prototype(
                    Some(prototype),
                    ObjectClass::Registry(registry),
                ))))
            }
            M::Register | M::Unregister => {
                let registry = match &this {
                    Value::Object(object) => match &object.borrow().class {
                        ObjectClass::Registry(registry) => Some(registry.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(registry) = registry else {
                    return Err(self.type_error(
                        "FinalizationRegistry method called on incompatible receiver",
                    ));
                };
                let token = argument(&arguments, if method == M::Register { 2 } else { 0 });
                let token = match token {
                    Value::Object(token) => Some(token),
                    Value::Undefined if method == M::Register => None,
                    _ => return Err(self.type_error("Invalid unregisterToken")),
                };
                if method == M::Unregister {
                    let token = token.expect("an object token");
                    let mut registry = registry.borrow_mut();
                    let before = registry.cells.len();
                    registry.cells.retain(|cell| {
                        cell.token
                            .as_ref()
                            .is_none_or(|held| held.as_ptr() != Rc::as_ptr(&token))
                    });
                    return Ok(Value::Bool(registry.cells.len() != before));
                }
                let Value::Object(target) = &first else {
                    return Err(
                        self.type_error("FinalizationRegistry.prototype.register: invalid target")
                    );
                };
                let held = argument(&arguments, 1);
                if matches!(&held, Value::Object(held) if Rc::ptr_eq(held, target)) {
                    return Err(self.type_error(
                        "FinalizationRegistry.prototype.register: target and holdings must not be same",
                    ));
                }
                registry.borrow_mut().cells.push(RegistryCell {
                    target: Rc::downgrade(target),
                    held,
                    token: token.as_ref().map(Rc::downgrade),
                });
                Ok(Value::Undefined)
            }
            M::WeakRefDeref => match &this {
                Value::Object(object) => match &object.borrow().class {
                    ObjectClass::WeakRef(target) => Ok(target
                        .upgrade()
                        .map(Value::Object)
                        .unwrap_or(Value::Undefined)),
                    _ => Err(runtime_error(
                        "WeakRef.prototype.deref called on incompatible receiver",
                    )),
                },
                _ => Err(runtime_error(
                    "WeakRef.prototype.deref called on incompatible receiver",
                )),
            }
            .map_err(|error| self.type_error(error.message)),
        }
    }
}

impl ScriptRuntime {
    // queues the cleanup callback for each registered target that is gone
    pub(super) fn sweep_registries(&mut self) {
        if self.registries.is_empty() {
            return;
        }
        self.registries
            .retain(|registry| registry.strong_count() > 0);
        let mut due = Vec::new();
        for registry in self.registries.iter().filter_map(Weak::upgrade) {
            let mut registry = registry.borrow_mut();
            let cleanup = registry.cleanup.clone();
            registry.cells.retain(|cell| {
                let alive = cell.target.strong_count() > 0;
                if !alive {
                    due.push((cleanup.clone(), cell.held.clone()));
                }
                alive
            });
        }
        for (cleanup, held) in due {
            self.schedule_callback(cleanup, vec![held]);
        }
    }
}
