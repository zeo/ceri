// Map, Set, WeakMap, WeakSet and WeakRef: insertion-ordered tables with a hash index keyed by
// SameValueZero identity, tombstones so live iterators see later additions and skip
// deletions, and weak keys that are pruned once their object is gone

use super::object::{JsObject, ObjectClass, ObjectHandle, SYMBOL_PREFIX, new_object};
use super::operations::{is_callable, is_primitive};
use super::*;
use crate::hashing::HashMap;
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
        Value::Object(handle) => handle.id(),
        Value::Array(handle) => handle.id(),
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
    weak_keys: Vec<Option<super::object::ObjectWeak>>,
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
            let weak = match &key {
                Value::Object(object) => Some(object.downgrade()),
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
    pub(super) fn prune(&mut self) {
        if !self.weak {
            return;
        }
        let dead: Vec<usize> = self
            .weak_keys
            .iter()
            .enumerate()
            .filter(|(_, key)| key.as_ref().is_some_and(|key| !key.is_alive()))
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

    pub(super) fn trace_strong(&self, tracer: &mut super::heap::Tracer) {
        for (key, value) in self.entries.iter().flatten() {
            tracer.trace_value(key);
            tracer.trace_value(value);
        }
    }

    pub(super) fn trace_non_object_keys(&self, tracer: &mut super::heap::Tracer) {
        for (position, entry) in self.entries.iter().enumerate() {
            if let Some((key, value)) = entry
                && self.weak_keys.get(position).and_then(|k| *k).is_none()
            {
                tracer.trace_value(key);
                tracer.trace_value(value);
            }
        }
    }

    pub(super) fn trace_weak_values(&self, tracer: &mut super::heap::Tracer) {
        for (position, entry) in self.entries.iter().enumerate() {
            if let Some((_, value)) = entry
                && let Some(Some(weak_key)) = self.weak_keys.get(position)
                && tracer.is_object_marked(*weak_key)
            {
                tracer.trace_value(value);
            }
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
    Get(CollectionKind),
    Set(CollectionKind),
    Add(CollectionKind),
    Has(CollectionKind),
    Delete(CollectionKind),
    Clear(CollectionKind),
    Size(CollectionKind),
    ForEach(CollectionKind),
    Iterate(CollectionKind, IterationKind),
    GetOrInsert(CollectionKind),
    GetOrInsertComputed(CollectionKind),
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

impl Registry {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        tracer.trace_value(&self.cleanup);
        for cell in &self.cells {
            tracer.trace_value(&cell.held);
        }
    }
}

struct RegistryCell {
    target: super::object::ObjectWeak,
    held: Value,
    token: Option<super::object::ObjectWeak>,
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

    fn check_weak_key(&mut self, key: &Value) -> Result<(), ScriptError> {
        match key {
            Value::Symbol(sym) => {
                if sym
                    .strip_prefix(SYMBOL_PREFIX)
                    .is_some_and(|rest| rest.starts_with("for:"))
                {
                    return Err(
                        self.type_error("Registered symbol not allowed as weak collection key")
                    );
                }
                Ok(())
            }
            other if is_primitive(other) => {
                Err(self.type_error("Invalid value used as weak collection key"))
            }
            _ => Ok(()),
        }
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

    // read size, has, and keys from a set-like record
    fn get_set_record(&mut self, other: &Value) -> Result<(Value, f64, Value, Value), ScriptError> {
        if is_primitive(other) {
            return Err(self.type_error("The argument must be a Set-like object"));
        }
        let raw_size = self.get_property(other, "size")?;
        let num_size = self.coerce_number(raw_size)?;
        if num_size.is_nan() {
            return Err(self.type_error("Set-like size must be a number"));
        }
        let int_size = if num_size.is_infinite() {
            num_size
        } else {
            num_size.trunc()
        };
        if int_size < 0.0 {
            return Err(self.range_error("Set-like size must be non-negative"));
        }
        let has = self.get_property(other, "has")?;
        if !is_callable(&has) {
            return Err(self.type_error("Set-like has must be callable"));
        }
        let keys = self.get_property(other, "keys")?;
        if !is_callable(&keys) {
            return Err(self.type_error("Set-like keys must be callable"));
        }
        Ok((other.clone(), int_size, has, keys))
    }

    fn get_keys_iterator(
        &mut self,
        set_obj: &Value,
        keys_fn: &Value,
    ) -> Result<(Value, Value), ScriptError> {
        let iterator = self.call_function(keys_fn.clone(), set_obj.clone(), Vec::new())?;
        if is_primitive(&iterator) {
            return Err(self.type_error("Iterator result is not an object"));
        }
        let next = self.get_property(&iterator, "next")?;
        Ok((iterator, next))
    }

    fn close_keys_iterator(&mut self, iterator: &Value) -> Result<(), ScriptError> {
        let method = self.get_property(iterator, "return")?;
        if matches!(method, Value::Undefined | Value::Null) {
            return Ok(());
        }
        let result = self.call_function(method, iterator.clone(), Vec::new())?;
        if is_primitive(&result) {
            return Err(self.type_error("iterator result is not an object"));
        }
        Ok(())
    }

    fn step_keys_iterator(
        &mut self,
        iterator: &Value,
        next_fn: &Value,
    ) -> Result<Option<Value>, ScriptError> {
        let result = self.call_function(next_fn.clone(), iterator.clone(), Vec::new())?;
        if is_primitive(&result) {
            return Err(self.type_error("Iterator result is not an object"));
        }
        if self.get_property(&result, "done")?.truthy() {
            return Ok(None);
        }
        Ok(Some(self.get_property(&result, "value")?))
    }

    fn copy_set_entries(source: &Rc<RefCell<Collection>>, target: &Rc<RefCell<Collection>>) {
        let items = source.borrow().values();
        let mut table = target.borrow_mut();
        for (key, _) in items {
            table.set(key, Value::Undefined);
        }
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
            M::Get(kind)
            | M::Set(kind)
            | M::Add(kind)
            | M::Has(kind)
            | M::Delete(kind)
            | M::Clear(kind)
            | M::Size(kind)
            | M::ForEach(kind)
            | M::Iterate(kind, _)
            | M::GetOrInsert(kind)
            | M::GetOrInsertComputed(kind) => {
                let name = match method {
                    M::Get(_) => "get",
                    M::Set(_) => "set",
                    M::Add(_) => "add",
                    M::Has(_) => "has",
                    M::Delete(_) => "delete",
                    M::Clear(_) => "clear",
                    M::Size(_) => "size",
                    M::ForEach(_) => "forEach",
                    M::Iterate(_, IterationKind::Keys) => "keys",
                    M::Iterate(_, IterationKind::Values) => "values",
                    M::Iterate(_, IterationKind::Entries) => "entries",
                    M::GetOrInsert(_) => "getOrInsert",
                    M::GetOrInsertComputed(_) => "getOrInsertComputed",
                    _ => unreachable!("handled above"),
                };
                let collection = self.collection_of(&this, kind, name)?;
                let weak = matches!(kind, CollectionKind::WeakMap | CollectionKind::WeakSet);
                match method {
                    M::Get(_) => Ok(collection.borrow().get(&first).unwrap_or(Value::Undefined)),
                    M::Has(_) => Ok(Value::Bool(collection.borrow().has(&first))),
                    M::Set(_) | M::Add(_) => {
                        if weak {
                            self.check_weak_key(&first)?;
                        }
                        let value = if matches!(method, M::Set(_)) {
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
                    M::Delete(_) => Ok(Value::Bool(collection.borrow_mut().delete(&first))),
                    M::Clear(_) => {
                        collection.borrow_mut().clear();
                        Ok(Value::Undefined)
                    }
                    M::Size(_) => Ok(Value::Number(collection.borrow().len() as f64)),
                    M::ForEach(_) => {
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
                    M::Iterate(_, iteration) => Ok(self.cursor_value(Cursor::new(
                        collection,
                        iteration,
                        kind == CollectionKind::Set,
                    ))),
                    M::GetOrInsert(_) => {
                        if weak {
                            self.check_weak_key(&first)?;
                        }
                        let key = match first {
                            Value::Number(n) if n == 0.0 && n.is_sign_negative() => {
                                Value::Number(0.0)
                            }
                            other => other,
                        };
                        if let Some(existing) = collection.borrow().get(&key) {
                            return Ok(existing);
                        }
                        let value = argument(&arguments, 1);
                        if collection.borrow().len() >= MAX_RUNTIME_ARRAY_LENGTH {
                            return Err(
                                self.range_error("collection exceeds the deterministic size limit")
                            );
                        }
                        collection.borrow_mut().set(key, value.clone());
                        Ok(value)
                    }
                    M::GetOrInsertComputed(_) => {
                        if weak {
                            self.check_weak_key(&first)?;
                        }
                        let callback = argument(&arguments, 1);
                        if !is_callable(&callback) {
                            return Err(
                                self.type_error("getOrInsertComputed callback is not a function")
                            );
                        }
                        let key = match first {
                            Value::Number(n) if n == 0.0 && n.is_sign_negative() => {
                                Value::Number(0.0)
                            }
                            other => other,
                        };
                        if let Some(existing) = collection.borrow().get(&key) {
                            return Ok(existing);
                        }
                        let value =
                            self.call_function(callback, Value::Undefined, vec![key.clone()])?;
                        if collection.borrow().len() >= MAX_RUNTIME_ARRAY_LENGTH {
                            return Err(
                                self.range_error("collection exceeds the deterministic size limit")
                            );
                        }
                        collection.borrow_mut().set(key, value.clone());
                        Ok(value)
                    }
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
                let mut index_of: HashMap<KeyId, usize> = HashMap::default();
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
                let (other_obj, other_size, other_has, other_keys) = self.get_set_record(&first)?;
                match method {
                    M::Union => {
                        let (iter, next) = self.get_keys_iterator(&other_obj, &other_keys)?;
                        let (set, result) = self.new_collection(CollectionKind::Set, None);
                        Self::copy_set_entries(&collection, &result);
                        while let Some(value) = self.step_keys_iterator(&iter, &next)? {
                            result.borrow_mut().set(value, Value::Undefined);
                        }
                        Ok(set)
                    }
                    M::Intersection => {
                        let (set, result) = self.new_collection(CollectionKind::Set, None);
                        let this_size = collection.borrow().len() as f64;
                        if this_size <= other_size {
                            let mut cursor =
                                Cursor::new(collection.clone(), IterationKind::Values, true);
                            loop {
                                let found = cursor.collection.borrow().entry_at(cursor.position);
                                let Some((index, elem, _)) = found else {
                                    break;
                                };
                                cursor.position = index + 1;
                                let in_other = self
                                    .call_function(
                                        other_has.clone(),
                                        other_obj.clone(),
                                        vec![elem.clone()],
                                    )?
                                    .truthy();
                                if in_other
                                    && collection.borrow().has(&elem)
                                    && !result.borrow().has(&elem)
                                {
                                    result.borrow_mut().set(elem, Value::Undefined);
                                }
                            }
                        } else {
                            let (iter, next) = self.get_keys_iterator(&other_obj, &other_keys)?;
                            while let Some(value) = self.step_keys_iterator(&iter, &next)? {
                                if collection.borrow().has(&value) && !result.borrow().has(&value) {
                                    result.borrow_mut().set(value, Value::Undefined);
                                }
                            }
                        }
                        Ok(set)
                    }
                    M::Difference => {
                        let (set, result) = self.new_collection(CollectionKind::Set, None);
                        Self::copy_set_entries(&collection, &result);
                        let this_size = collection.borrow().len() as f64;
                        if this_size <= other_size {
                            let mut cursor =
                                Cursor::new(result.clone(), IterationKind::Values, true);
                            loop {
                                let found = cursor.collection.borrow().entry_at(cursor.position);
                                let Some((index, elem, _)) = found else {
                                    break;
                                };
                                cursor.position = index + 1;
                                let in_other = self
                                    .call_function(
                                        other_has.clone(),
                                        other_obj.clone(),
                                        vec![elem.clone()],
                                    )?
                                    .truthy();
                                if in_other {
                                    result.borrow_mut().delete(&elem);
                                }
                            }
                        } else {
                            let (iter, next) = self.get_keys_iterator(&other_obj, &other_keys)?;
                            while let Some(value) = self.step_keys_iterator(&iter, &next)? {
                                result.borrow_mut().delete(&value);
                            }
                        }
                        Ok(set)
                    }
                    M::SymmetricDifference => {
                        let (iter, next) = self.get_keys_iterator(&other_obj, &other_keys)?;
                        let (set, result) = self.new_collection(CollectionKind::Set, None);
                        Self::copy_set_entries(&collection, &result);
                        while let Some(value) = self.step_keys_iterator(&iter, &next)? {
                            let in_result = result.borrow().has(&value);
                            if collection.borrow().has(&value) {
                                if in_result {
                                    result.borrow_mut().delete(&value);
                                }
                            } else if !in_result {
                                result.borrow_mut().set(value, Value::Undefined);
                            }
                        }
                        Ok(set)
                    }
                    M::IsSubsetOf => {
                        if (collection.borrow().len() as f64) > other_size {
                            return Ok(Value::Bool(false));
                        }
                        let mut cursor =
                            Cursor::new(collection.clone(), IterationKind::Values, true);
                        loop {
                            let found = cursor.collection.borrow().entry_at(cursor.position);
                            let Some((index, elem, _)) = found else {
                                break;
                            };
                            cursor.position = index + 1;
                            let in_other = self
                                .call_function(other_has.clone(), other_obj.clone(), vec![elem])?
                                .truthy();
                            if !in_other {
                                return Ok(Value::Bool(false));
                            }
                        }
                        Ok(Value::Bool(true))
                    }
                    M::IsSupersetOf => {
                        if (collection.borrow().len() as f64) < other_size {
                            return Ok(Value::Bool(false));
                        }
                        let (iter, next) = self.get_keys_iterator(&other_obj, &other_keys)?;
                        while let Some(value) = self.step_keys_iterator(&iter, &next)? {
                            if !collection.borrow().has(&value) {
                                self.close_keys_iterator(&iter)?;
                                return Ok(Value::Bool(false));
                            }
                        }
                        Ok(Value::Bool(true))
                    }
                    M::IsDisjointFrom => {
                        if (collection.borrow().len() as f64) <= other_size {
                            let mut cursor =
                                Cursor::new(collection.clone(), IterationKind::Values, true);
                            loop {
                                let found = cursor.collection.borrow().entry_at(cursor.position);
                                let Some((index, elem, _)) = found else {
                                    break;
                                };
                                cursor.position = index + 1;
                                let in_other = self
                                    .call_function(
                                        other_has.clone(),
                                        other_obj.clone(),
                                        vec![elem],
                                    )?
                                    .truthy();
                                if in_other {
                                    return Ok(Value::Bool(false));
                                }
                            }
                        } else {
                            let (iter, next) = self.get_keys_iterator(&other_obj, &other_keys)?;
                            while let Some(value) = self.step_keys_iterator(&iter, &next)? {
                                if collection.borrow().has(&value) {
                                    self.close_keys_iterator(&iter)?;
                                    return Ok(Value::Bool(false));
                                }
                            }
                        }
                        Ok(Value::Bool(true))
                    }
                    _ => unreachable!("handled above"),
                }
            }
            M::WeakRefConstructor => {
                if new_target.is_none() {
                    return Err(self.type_error("Constructor WeakRef requires 'new'"));
                }
                let Value::Object(target) = &first else {
                    return Err(self.type_error("WeakRef: target must be an object"));
                };
                self.has_weak_refs.set(true);
                let prototype = self.intrinsics.weak_ref_prototype.clone();
                Ok(Value::Object(new_object(JsObject::with_prototype(
                    Some(prototype),
                    ObjectClass::WeakRef(target.downgrade()),
                ))))
            }
            M::RegistryConstructor => {
                if new_target.is_none() {
                    return Err(self.type_error("Constructor FinalizationRegistry requires 'new'"));
                }
                if !is_callable(&first) {
                    return Err(self.type_error("FinalizationRegistry: cleanup must be callable"));
                }
                self.has_weak_refs.set(true);
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
                        cell.token.as_ref().is_none_or(|held| !held.matches(&token))
                    });
                    return Ok(Value::Bool(registry.cells.len() != before));
                }
                let Value::Object(target) = &first else {
                    return Err(
                        self.type_error("FinalizationRegistry.prototype.register: invalid target")
                    );
                };
                let held = argument(&arguments, 1);
                if matches!(&held, Value::Object(held) if ObjectHandle::ptr_eq(held, target)) {
                    return Err(self.type_error(
                        "FinalizationRegistry.prototype.register: target and holdings must not be same",
                    ));
                }
                registry.borrow_mut().cells.push(RegistryCell {
                    target: target.downgrade(),
                    held,
                    token: token.as_ref().map(ObjectHandle::downgrade),
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
                let alive = cell.target.is_alive();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collections_enforce_brand_checks_weak_symbols_and_set_methods() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r#"
                const seen = [];
                for (const fn of [
                    () => Map.prototype.get.call({}, "a"),
                    () => Set.prototype.has.call({}, 1),
                    () => WeakMap.prototype.get.call({}, {}),
                    () => WeakSet.prototype.has.call({}, {}),
                ]) {
                    try { fn(); seen.push("ok"); } catch (e) { seen.push(e.name); }
                }
                const wm = new WeakMap();
                const sym = Symbol("k");
                wm.set(sym, 42);
                seen.push(wm.get(sym));
                try { wm.set(Symbol.for("reg"), 1); seen.push("ok"); } catch (e) { seen.push(e.name); }
                const m = new Map([["a", 1]]);
                seen.push(m.getOrInsert("a", 9), m.getOrInsert("b", 2), m.getOrInsertComputed("c", (k) => k + "!"));
                let closed = false;
                const setLike = {
                    size: 1,
                    has() { return false; },
                    keys() {
                        let i = 0;
                        return {
                            next() { return i++ === 0 ? { value: 2, done: false } : { value: undefined, done: true }; },
                            return() { closed = true; return { done: true }; }
                        };
                    }
                };
                const a = new Set([1, 2]);
                seen.push([...a.union(setLike)].join(), a.isDisjointFrom(setLike), closed);
                document.getElementById("out").textContent = seen.join("|");
                "#,
            )
            .expect("script should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some("TypeError|TypeError|TypeError|TypeError|42|TypeError|1|2|c!|1,2|false|true")
        );
    }
}
