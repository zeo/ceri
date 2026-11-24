// ordinary objects: insertion-ordered properties with attributes and accessors, a prototype
// link and an internal class slot for functions, boxed primitives and other exotic kinds

use super::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

pub(super) type ObjectHandle = Rc<RefCell<JsObject>>;

// a property key hash a word at a time, far quicker than the standard one on the short keys
// scripts use, and seeded once per process so a page cannot line its keys up on purpose
#[derive(Clone, Copy)]
pub(super) struct KeyHasher(u64);

impl Default for KeyHasher {
    fn default() -> Self {
        static SEED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        Self(*SEED.get_or_init(|| {
            use std::hash::{BuildHasher, Hasher};
            std::collections::hash_map::RandomState::new()
                .build_hasher()
                .finish()
        }))
    }
}

impl std::hash::Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(word))
                .wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
}

pub(super) type KeyMap<V> = HashMap<Rc<str>, V, std::hash::BuildHasherDefault<KeyHasher>>;

#[derive(Clone)]
pub(super) enum Slot {
    Data(Value),
    // boxed, as accessors are rare and a data slot then takes no more room than its value
    Accessor(Box<Accessors>),
}

#[derive(Clone)]
pub(super) struct Accessors {
    pub(super) get: Option<Value>,
    pub(super) set: Option<Value>,
}

impl Slot {
    pub(super) fn accessor(get: Option<Value>, set: Option<Value>) -> Self {
        Self::Accessor(Box::new(Accessors { get, set }))
    }
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
    // boxed with the rest of the rare kinds kept small, as every object carries its class
    Bound(Box<BoundFunction>),
    Arguments,
    // what JSON.rawJSON makes, which JSON.stringify writes out as its text
    RawJson,
    Locale(Rc<super::locale::LocaleTag>),
    DisplayNames(Rc<super::displaynames::DisplayOptions>),
    DurationFormat(Rc<super::durationformat::DurationOptions>),
    // an OffscreenCanvas and the canvas element it keeps alive
    Offscreen(super::ElementHandle),
    UrlPattern(Rc<super::urlpattern::UrlPattern>),
    // an XPathResult, XPathExpression or XPathEvaluator
    XPath(Rc<super::xpath::XPathObject>),
    // a Typed OM value, variable reference, numeric array or style property map
    StyleOm(Rc<super::typed_om::StyleOm>),
    // a WebGL context, one of its objects, or an extension, active info or precision format
    WebGl(Rc<super::webgl::GlObject>),
    // a value of one of the Temporal types
    Temporal(Rc<super::temporal::Temporal>),
    PerfObserver(Rc<super::perf_observers::PerfObserver>),
    // the entries a PerformanceObserver callback is handed
    PerfEntries(Rc<Vec<super::geometry::PerformanceEntry>>),
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
        tracking: bool,
    },
    TextDecoder(Rc<RefCell<super::encoding::DecoderState>>),
    Intl(Rc<super::intl::IntlState>),
    Url(Rc<RefCell<super::urls::UrlState>>),
    UrlSearchParams(Rc<RefCell<super::urls::ParamsState>>),
    // a TreeWalker or NodeIterator
    Walk(Rc<RefCell<super::traversal::WalkState>>),
    // the object of a promise value, which keeps its expandos and its prototype
    Promise,
    Registry(Rc<RefCell<super::collections::Registry>>),
    // a Path2D and the calls that made it
    Path(super::path2d::PathHandle),
    Matrix(Rc<RefCell<super::matrices::MatrixState>>),
    FontFace(Rc<super::fontfaces::FaceState>),
    // a CSSStyleSheet, and a rule it lists
    Sheet(Rc<super::cssom::SheetState>),
    CssRule(Rc<super::cssom::RuleState>),
    // an Intl.Segmenter, the segments of a string, or an iterator over them
    Segmenter(Rc<super::segmenter::SegmenterState>),
    DisposableStack(Rc<super::disposables::DisposableStack>),
    // a stream, its controller, a reader or an iterator over one
    Stream(super::streams::StreamHandle),
    // the object of a typed array of this kind, for its named properties and prototype
    TypedArray(super::typed::ElementKind),
    // what an iterator helper such as map or take hands back
    IteratorHelper(Rc<RefCell<super::iterator_helpers::HelperState>>),
}

// the keys of an object in insertion order. objects that gained the same keys in the same order
// share one, so each keeps only its property values, and a key added moves an object to the
// shape one key longer. an object that loses a key or grows large gets a shape of its own
struct Shape {
    keys: Vec<Rc<str>>,
    // built once a shape grows past a handful of keys
    index: Option<KeyMap<usize>>,
    // the shapes one key longer, while some object still has them
    transitions: RefCell<KeyMap<Weak<Shape>>>,
    // the shape one key shorter, kept alive so the next object to take the same keys finds
    // the same path
    _parent: Option<Rc<Shape>>,
    // the most keys any shape grown from this one has, so an object that must grow its
    // properties can make room for as many as objects made like it ended up with
    longest: std::cell::Cell<usize>,
    // a shape of one object's own, changed in place
    own: bool,
}

// keys past which an object stops sharing its shape, so one used as a map makes no chain of
// shapes as long as itself
const SHARED_KEYS: usize = 64;

impl Shape {
    fn root() -> Rc<Self> {
        thread_local! {
            static ROOT: Rc<Shape> = Rc::new(Shape {
                keys: Vec::new(),
                index: None,
                transitions: RefCell::default(),
                _parent: None,
                longest: std::cell::Cell::new(0),
                own: false,
            });
        }
        ROOT.with(Rc::clone)
    }

    fn own_copy(&self) -> Self {
        Self {
            keys: self.keys.clone(),
            index: self.index.clone(),
            transitions: RefCell::default(),
            _parent: None,
            longest: std::cell::Cell::new(self.keys.len()),
            own: true,
        }
    }

    fn position(&self, key: &str) -> Option<usize> {
        match &self.index {
            Some(index) => index.get(key).copied(),
            None => self.keys.iter().position(|name| &**name == key),
        }
    }

    fn reindex(&mut self) {
        self.index = (self.keys.len() > INDEX_THRESHOLD).then(|| {
            self.keys
                .iter()
                .enumerate()
                .map(|(position, key)| (key.clone(), position))
                .collect()
        });
    }

    fn push_own(&mut self, key: Rc<str>) {
        if let Some(index) = &mut self.index {
            index.insert(key.clone(), self.keys.len());
        }
        self.keys.push(key);
        if self.index.is_none() && self.keys.len() > INDEX_THRESHOLD {
            self.reindex();
        }
    }

    // the shared shape with one key more, made once and found again by every object after
    fn with_key(self: &Rc<Self>, key: &str) -> Rc<Self> {
        let mut transitions = self.transitions.borrow_mut();
        if let Some(next) = transitions.get(key).and_then(Weak::upgrade) {
            return next;
        }
        if transitions.len() >= 16 {
            transitions.retain(|_, next| next.strong_count() > 0);
        }
        let key: Rc<str> = Rc::from(key);
        let mut next = Shape {
            keys: self.keys.clone(),
            index: self.index.clone(),
            transitions: RefCell::default(),
            _parent: Some(self.clone()),
            longest: std::cell::Cell::new(self.keys.len() + 1),
            own: false,
        };
        next.push_own(key.clone());
        let next = Rc::new(next);
        transitions.insert(key, Rc::downgrade(&next));
        let length = next.keys.len();
        let mut shape = Some(self);
        while let Some(ancestor) = shape {
            ancestor.longest.set(ancestor.longest.get().max(length));
            shape = ancestor._parent.as_ref();
        }
        next
    }
}

// what Function.prototype.bind made
#[derive(Clone)]
pub(super) struct BoundFunction {
    pub(super) target: Value,
    pub(super) this: Value,
    pub(super) arguments: Vec<Value>,
}

// where a property name was last found on objects of one shared shape, so the next read of it
// through the same code skips the search; it holds the shape weakly, which keeps its address
// from going to another shape while the entry stands
#[derive(Default)]
pub(super) struct PropertyCache(RefCell<Option<Cached>>);

enum Cached {
    // an own data property of objects of one shared shape
    Own(Weak<Shape>, usize),
    // a data property of the prototype one step up: the shape of the objects it was read
    // through, none for an array with nothing of its own, then the prototype, its shape, and
    // where the property sits on it
    Inherited {
        receiver: Option<Weak<Shape>>,
        holder: Weak<RefCell<JsObject>>,
        shape: Weak<Shape>,
        index: usize,
    },
}

impl std::fmt::Debug for PropertyCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PropertyCache")
    }
}
// an object's property values, or ones it holds in common with others made alike until it
// changes them, as the many closures of one function do with their length and name
#[derive(Clone)]
enum Properties {
    Own(Vec<Property>),
    Shared(Rc<[Property]>),
}

impl Default for Properties {
    fn default() -> Self {
        Self::Own(Vec::new())
    }
}

impl std::ops::Deref for Properties {
    type Target = [Property];

    fn deref(&self) -> &[Property] {
        match self {
            Self::Own(own) => own,
            Self::Shared(shared) => shared,
        }
    }
}

impl Properties {
    fn own(&mut self) -> &mut Vec<Property> {
        if let Self::Shared(shared) = self {
            *self = Self::Own(shared.to_vec());
        }
        match self {
            Self::Own(own) => own,
            Self::Shared(_) => unreachable!("made its own just above"),
        }
    }

    fn capacity(&self) -> usize {
        match self {
            Self::Own(own) => own.capacity(),
            Self::Shared(shared) => shared.len(),
        }
    }
}

// the keys and values objects made alike start with, which each takes without copying
pub(super) struct ObjectTemplate {
    shape: Rc<Shape>,
    properties: Rc<[Property]>,
}

impl std::fmt::Debug for ObjectTemplate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ObjectTemplate")
    }
}

#[derive(Clone)]
pub(super) struct JsObject {
    shape: Rc<Shape>,
    // the values of the shape's keys, in the same order
    properties: Properties,
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
            shape: Shape::root(),
            properties: Properties::default(),
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

    // an object with another's keys, prototype and class and these properties in the same
    // order, which shares the other's shape instead of building one key by key
    pub(super) fn with_shape_of(template: &JsObject, properties: Vec<Property>) -> Self {
        debug_assert_eq!(template.properties.len(), properties.len());
        Self {
            shape: template.shape.clone(),
            properties: Properties::Own(properties),
            prototype: template.prototype.clone(),
            orphan: template.orphan,
            extensible: template.extensible,
            class: template.class.clone(),
        }
    }

    // the own data value the cache points at, when the object still has the shape it saw
    pub(super) fn cached_value(&self, cache: &PropertyCache) -> Option<Value> {
        // a proxy answers through its traps, never from what it holds
        if matches!(self.class, ObjectClass::Proxy(_)) {
            return None;
        }
        let cached = cache.0.borrow();
        match cached.as_ref()? {
            Cached::Own(shape, index) => {
                if !std::ptr::eq(shape.as_ptr(), Rc::as_ptr(&self.shape)) {
                    return None;
                }
                self.properties.get(*index)?.value().map(Value::resolve)
            }
            Cached::Inherited {
                receiver: Some(receiver),
                holder,
                shape,
                index,
            } => {
                // the same shape has the same own keys, so the read still passes through to
                // the prototype, if that is still the one noted
                if !std::ptr::eq(receiver.as_ptr(), Rc::as_ptr(&self.shape)) {
                    return None;
                }
                let prototype = self.prototype.as_ref()?;
                if !std::ptr::eq(Rc::as_ptr(prototype), holder.as_ptr()) {
                    return None;
                }
                Self::held(prototype, shape, *index)
            }
            Cached::Inherited { receiver: None, .. } => None,
        }
    }

    // writes an ordinary object's own writable data property where the cache noted it, which
    // a write to it settles without looking up the prototypes for a setter; the value comes
    // back when the write needs the full path
    pub(super) fn cached_set(&mut self, cache: &PropertyCache, value: Value) -> Result<(), Value> {
        if !matches!(self.class, ObjectClass::Ordinary) {
            return Err(value);
        }
        let index = match cache.0.borrow().as_ref() {
            Some(Cached::Own(shape, index))
                if std::ptr::eq(shape.as_ptr(), Rc::as_ptr(&self.shape)) =>
            {
                *index
            }
            _ => return Err(value),
        };
        let plain = matches!(
            self.properties.get(index),
            Some(Property {
                slot: Slot::Data(current),
                writable: true,
                ..
            }) if !matches!(current, Value::Binding(_) | Value::ConstBinding(_))
        );
        if !plain {
            return Err(value);
        }
        if let Slot::Data(slot) = &mut self.properties.own()[index].slot {
            *slot = value;
        }
        Ok(())
    }

    // a method read off Array.prototype through an array with no prototype or property of
    // its own
    pub(super) fn cached_array_method(
        prototype: &ObjectHandle,
        cache: &PropertyCache,
    ) -> Option<Value> {
        match cache.0.borrow().as_ref()? {
            Cached::Inherited {
                receiver: None,
                holder,
                shape,
                index,
            } if std::ptr::eq(Rc::as_ptr(prototype), holder.as_ptr()) => {
                Self::held(prototype, shape, *index)
            }
            _ => None,
        }
    }

    fn held(holder: &ObjectHandle, shape: &Weak<Shape>, index: usize) -> Option<Value> {
        let holder = holder.try_borrow().ok()?;
        if matches!(holder.class, ObjectClass::Proxy(_))
            || !std::ptr::eq(shape.as_ptr(), Rc::as_ptr(&holder.shape))
        {
            return None;
        }
        holder.properties.get(index)?.value().map(Value::resolve)
    }

    // notes where the key sits on this object's shape, or on its prototype's when it has none
    // of its own; an own shape changes in place and is never noted
    pub(super) fn note_position(&self, key: &str, cache: &PropertyCache) {
        if self.shape.own || matches!(self.class, ObjectClass::Proxy(_)) {
            return;
        }
        if let Some(index) = self.shape.position(key) {
            *cache.0.borrow_mut() = Some(Cached::Own(Rc::downgrade(&self.shape), index));
        } else if let Some(prototype) = &self.prototype {
            Self::note_inherited(Some(Rc::downgrade(&self.shape)), prototype, key, cache);
        }
    }

    // notes where a method sits on Array.prototype, for arrays with nothing of their own
    pub(super) fn note_array_method(prototype: &ObjectHandle, key: &str, cache: &PropertyCache) {
        Self::note_inherited(None, prototype, key, cache);
    }

    fn note_inherited(
        receiver: Option<Weak<Shape>>,
        holder: &ObjectHandle,
        key: &str,
        cache: &PropertyCache,
    ) {
        let Ok(object) = holder.try_borrow() else {
            return;
        };
        if matches!(object.class, ObjectClass::Proxy(_)) {
            return;
        }
        if let Some(index) = object.shape.position(key)
            && object.properties[index].value().is_some()
        {
            *cache.0.borrow_mut() = Some(Cached::Inherited {
                receiver,
                holder: Rc::downgrade(holder),
                shape: Rc::downgrade(&object.shape),
                index,
            });
        }
    }
    // the keys and values this object has, for others made alike to start from; it then holds
    // its values in common with them, and an object with a shape of its own makes none
    pub(super) fn template(&mut self) -> Option<ObjectTemplate> {
        if self.shape.own {
            return None;
        }
        let shared: Rc<[Property]> = match &self.properties {
            Properties::Shared(shared) => shared.clone(),
            Properties::Own(own) => Rc::from(own.as_slice()),
        };
        self.properties = Properties::Shared(shared.clone());
        Some(ObjectTemplate {
            shape: self.shape.clone(),
            properties: shared,
        })
    }

    pub(super) fn from_template(
        template: &ObjectTemplate,
        prototype: Option<ObjectHandle>,
        class: ObjectClass,
    ) -> Self {
        Self {
            shape: template.shape.clone(),
            properties: Properties::Shared(template.properties.clone()),
            prototype,
            orphan: false,
            extensible: true,
            class,
        }
    }

    // whether the object's own keys are these, in this order
    pub(super) fn has_keys<'a>(&self, keys: impl ExactSizeIterator<Item = &'a str>) -> bool {
        keys.len() == self.shape.keys.len()
            && keys.zip(&self.shape.keys).all(|(key, own)| key == &**own)
    }
    // room for exactly this many properties, for objects whose properties are known when made
    pub(super) fn reserve(&mut self, properties: usize) {
        self.properties.own().reserve_exact(properties);
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
                ObjectClass::Promise => intrinsics.promise_prototype.clone(),
                ObjectClass::TypedArray(kind) => {
                    let index = super::typed::ELEMENT_KINDS
                        .iter()
                        .position(|(candidate, _)| candidate == kind)
                        .unwrap_or(0);
                    intrinsics.typed_prototypes[index].clone()
                }
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
        self.shape.position(key)
    }

    // the object's own shape, made from the shared one the first time it is changed in place
    fn own_shape(&mut self) -> &mut Shape {
        // a cloned object holds the same own shape until one of the two changes it, and one a
        // cache has noted changes into a copy at a new address, so the cache sees it is not
        // the shape it noted
        if !self.shape.own || Rc::strong_count(&self.shape) > 1 || Rc::weak_count(&self.shape) > 0 {
            self.shape = Rc::new(self.shape.own_copy());
        }
        Rc::get_mut(&mut self.shape).expect("an own shape has no other holder")
    }

    pub(super) fn property(&self, key: &str) -> Option<&Property> {
        self.position(key)
            .map(|position| &self.properties[position])
    }

    pub(super) fn property_mut(&mut self, key: &str) -> Option<&mut Property> {
        self.position(key)
            .map(move |position| &mut self.properties.own()[position])
    }

    // defines or replaces a property with exact attributes
    pub(super) fn define(&mut self, key: String, property: Property) {
        self.define_key(&key, property);
    }

    // the same with a borrowed key, which a shared shape that already has it never copies
    pub(super) fn define_key(&mut self, key: &str, property: Property) {
        if let Some(position) = self.position(key) {
            self.properties.own()[position] = property;
            return;
        }
        if self.shape.own || self.properties.len() >= SHARED_KEYS {
            self.own_shape().push_own(Rc::from(key));
        } else {
            self.shape = self.shape.with_key(key);
            let length = self.properties.len();
            if length == self.properties.capacity() {
                // never more than doubling would take, so a shape shared by unlike objects only
                // ever saves room
                let doubled = (length * 2).max(4);
                let wanted = self.shape.longest.get().clamp(length + 1, doubled);
                self.properties.own().reserve_exact(wanted - length);
            }
        }
        self.properties.own().push(property);
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
        let property = self.properties.own().remove(position);
        let shape = self.own_shape();
        shape.keys.remove(position);
        shape.reindex();
        property.value().cloned()
    }

    // own property keys in spec order: integer keys ascending, then strings in insertion order
    pub(super) fn ordered_keys(&self) -> Vec<String> {
        let mut integers = Vec::new();
        let mut strings = Vec::new();
        for key in &self.shape.keys {
            match array_index(key) {
                Some(index) => integers.push((index, key.to_string())),
                None => strings.push(key.to_string()),
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

    // the own properties in insertion order, without their keys
    pub(super) fn properties(&self) -> &[Property] {
        &self.properties
    }

    // the prototype set on the object itself, which the cycle collector follows
    pub(super) fn own_prototype(&self) -> Option<&ObjectHandle> {
        self.prototype.as_ref()
    }

    // empties an object the cycle collector found unreachable: what it held, and its class
    pub(super) fn take_for_collection(&mut self) -> (Vec<Value>, ObjectClass) {
        let mut values = Vec::new();
        for property in std::mem::take(&mut self.properties).iter().cloned() {
            match property.slot {
                Slot::Data(value) => values.push(value),
                Slot::Accessor(accessors) => {
                    values.extend(accessors.get.into_iter().chain(accessors.set))
                }
            }
        }
        self.shape = Shape::root();
        if let Some(prototype) = self.prototype.take() {
            values.push(Value::Object(prototype));
        }
        (
            values,
            std::mem::replace(&mut self.class, ObjectClass::Ordinary),
        )
    }

    pub(super) fn freeze(&mut self, writable: bool) {
        self.extensible = false;
        for property in self.properties.own() {
            property.configurable = false;
            if !writable && matches!(property.slot, Slot::Data(_)) {
                property.writable = false;
            }
        }
    }

    pub(super) fn is_callable(&self) -> bool {
        match &self.class {
            ObjectClass::Function(_) | ObjectClass::Bound(_) | ObjectClass::Native(_) => true,
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
    let handle = Rc::new(RefCell::new(object));
    super::heap::register_object(&handle);
    handle
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
    fn an_object_and_a_property_stay_small_enough_for_heaps_of_them() {
        assert!(std::mem::size_of::<JsObject>() <= 80);
        assert!(std::mem::size_of::<Property>() <= 32);
        assert!(std::mem::size_of::<Value>() <= 24);
    }

    #[test]
    fn an_object_made_like_others_takes_room_for_exactly_their_keys() {
        let made = || {
            let mut object = JsObject::new();
            for key in [
                "fn",
                "state",
                "updatedAt",
                "owned",
                "sources",
                "sourceSlots",
            ] {
                object.insert(key.to_string(), Value::Null);
            }
            for key in ["cleanups", "value", "owner", "context", "pure"] {
                object.insert(key.to_string(), Value::Null);
            }
            object
        };
        let _first = made();
        let second = made();
        assert_eq!(second.properties.capacity(), 11);
        let mut small = JsObject::new();
        small.insert("fn".to_string(), Value::Null);
        assert!(small.properties.capacity() <= 4);
    }

    #[test]
    fn objects_given_the_same_keys_in_the_same_order_share_one_shape() {
        let made = || {
            let mut object = JsObject::new();
            for key in ["fn", "state", "value"] {
                object.insert(key.to_string(), Value::Null);
            }
            object
        };
        let (first, second) = (made(), made());
        assert!(Rc::ptr_eq(&first.shape, &second.shape));
        let mut third = made();
        third.remove("state");
        assert!(!Rc::ptr_eq(&first.shape, &third.shape));
        assert_eq!(third.ordered_keys(), ["fn", "value"]);
        assert_eq!(first.ordered_keys(), ["fn", "state", "value"]);
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
