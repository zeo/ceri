// ordinary objects: insertion-ordered properties with attributes and accessors, a prototype
// link and an internal class slot for functions, boxed primitives and other exotic kinds

use super::Value;
pub(super) use super::heap::{ObjectHandle, ObjectWeak};
use std::cell::{RefCell, UnsafeCell};
use std::rc::{Rc, Weak};

pub(super) const ATTR_ENUMERABLE: u8 = 1;
pub(super) const ATTR_WRITABLE: u8 = 2;
pub(super) const ATTR_CONFIGURABLE: u8 = 4;
pub(super) const ATTR_ACCESSOR: u8 = 8;
pub(super) const ATTR_DATA: u8 = ATTR_ENUMERABLE | ATTR_WRITABLE | ATTR_CONFIGURABLE;

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

pub(super) type KeyMap<V> =
    std::collections::HashMap<Rc<str>, V, std::hash::BuildHasherDefault<KeyHasher>>;

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

    #[inline]
    pub(super) fn value(&self) -> Option<&Value> {
        match self {
            Self::Data(value) => Some(value),
            Self::Accessor(_) => None,
        }
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

    pub(super) fn attrs(&self) -> u8 {
        let mut bits = 0u8;
        if self.enumerable {
            bits |= ATTR_ENUMERABLE;
        }
        if self.writable {
            bits |= ATTR_WRITABLE;
        }
        if self.configurable {
            bits |= ATTR_CONFIGURABLE;
        }
        if matches!(self.slot, Slot::Accessor(_)) {
            bits |= ATTR_ACCESSOR;
        }
        bits
    }
}

#[derive(Clone, Copy)]
pub(super) struct PropertyRef<'a> {
    pub(super) slot: &'a Slot,
    pub(super) enumerable: bool,
    pub(super) writable: bool,
    pub(super) configurable: bool,
}

impl<'a> PropertyRef<'a> {
    #[inline]
    pub(super) fn value(&self) -> Option<&'a Value> {
        self.slot.value()
    }

    pub(super) fn to_property(self) -> Property {
        Property {
            slot: self.slot.clone(),
            enumerable: self.enumerable,
            writable: self.writable,
            configurable: self.configurable,
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

// a DataView's buffer and the bytes it covers, boxed as a rare class kept narrow
#[derive(Clone)]
pub(super) struct DataViewRange {
    pub(super) buffer: super::BytesHandle,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) tracking: bool,
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
    String(super::JsStr),
    Symbol(super::JsStr),
    BigInt(Rc<super::bigint::BigInt>),
    Error,
    Generator(Rc<RefCell<super::vm::GeneratorState>>),
    AsyncGenerator(Rc<RefCell<super::vm::AsyncGeneratorData>>),
    // a function implemented by the engine, such as Object.keys
    // behind a pointer, as the widest natives would otherwise widen every object
    Native(Rc<super::realm::Native>),
    RegExp(Rc<super::pattern::RegExpData>),
    Proxy(Rc<super::reflect::ProxyData>),
    Date(f64),
    Map(Rc<RefCell<super::collections::Collection>>),
    Set(Rc<RefCell<super::collections::Collection>>),
    WeakMap(Rc<RefCell<super::collections::Collection>>),
    WeakSet(Rc<RefCell<super::collections::Collection>>),
    WeakRef(ObjectWeak),
    // an Array Iterator, which reads its target live on every step
    ArrayIterator(Rc<RefCell<ArrayIteration>>),
    // an object that holds event listeners, keyed by this id in the runtime's registry
    EventTarget(u32),
    // a Mutation, Resize or IntersectionObserver, keyed in the runtime's registry
    Observer(u32),
    // a live Map or Set iterator
    Cursor(Rc<RefCell<super::collections::Cursor>>),
    DataView(Box<DataViewRange>),
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

type AttrTransitions = Vec<(Rc<str>, u8, Weak<Shape>)>;

// the keys and attributes of an object in insertion order. objects that gained the same keys
// with the same attributes in the same order share one, so each keeps only compact 16-byte slots
pub(super) struct Shape {
    keys: Vec<Rc<str>>,
    attrs: Vec<u8>,
    // built once a shape grows past a handful of keys
    index: Option<KeyMap<usize>>,
    // the shapes one key longer with default data attributes, while some object still has them
    transitions: RefCell<KeyMap<Weak<Shape>>>,
    // shapes one key longer with non-default attributes (hidden, accessor, read-only)
    attr_transitions: RefCell<AttrTransitions>,
    // the shape one key shorter, kept alive so the next object to take the same keys finds
    // the same path
    _parent: Option<Rc<Shape>>,
    // the most keys any shape grown from this one has, so an object that must grow its
    // properties can make room for as many as objects made like it ended up with
    longest: std::cell::Cell<usize>,
    // a shape of one object's own, changed in place
    own: bool,
    // true if any non-symbol non-private key on this shape is enumerable
    has_enumerable: bool,
    // true if any key on this shape is an array index
    has_integer_key: bool,
}

impl std::fmt::Debug for Shape {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Shape")
    }
}

// keys past which an object stops sharing its shape, so one used as a map makes no chain of
// shapes as long as itself
pub(super) const SHARED_KEYS: usize = 64;

impl Shape {
    fn root() -> Rc<Self> {
        thread_local! {
            static ROOT: Rc<Shape> = Rc::new(Shape {
                keys: Vec::new(),
                attrs: Vec::new(),
                index: None,
                transitions: RefCell::default(),
                attr_transitions: RefCell::default(),
                _parent: None,
                longest: std::cell::Cell::new(0),
                own: false,
                has_enumerable: false,
                has_integer_key: false,
            });
        }
        ROOT.with(Rc::clone)
    }

    pub(super) fn from_keys(keys: &[&str]) -> Rc<Self> {
        let mut shape = Self::root();
        for key in keys {
            shape = shape.with_key_attrs(key, ATTR_DATA);
        }
        shape
    }

    pub(super) fn len(&self) -> usize {
        self.keys.len()
    }

    fn own_copy(&self) -> Self {
        Self {
            keys: self.keys.clone(),
            attrs: self.attrs.clone(),
            index: self.index.clone(),
            transitions: RefCell::default(),
            attr_transitions: RefCell::default(),
            _parent: None,
            longest: std::cell::Cell::new(self.keys.len()),
            own: true,
            has_enumerable: self.has_enumerable,
            has_integer_key: self.has_integer_key,
        }
    }

    #[inline]
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

    fn recompute_flags(&mut self) {
        self.has_enumerable = self.keys.iter().zip(&self.attrs).any(|(key, &attr)| {
            (attr & ATTR_ENUMERABLE) != 0 && !is_symbol_key(key) && !is_private_key(key)
        });
        self.has_integer_key = self.keys.iter().any(|key| array_index(key).is_some());
    }

    fn push_own(&mut self, key: Rc<str>, attrs: u8) {
        if (attrs & ATTR_ENUMERABLE) != 0 && !is_symbol_key(&key) && !is_private_key(&key) {
            self.has_enumerable = true;
        }
        if !self.has_integer_key && array_index(&key).is_some() {
            self.has_integer_key = true;
        }
        if let Some(index) = &mut self.index {
            index.insert(key.clone(), self.keys.len());
        }
        self.keys.push(key);
        self.attrs.push(attrs);
        if self.index.is_none() && self.keys.len() > INDEX_THRESHOLD {
            self.reindex();
        }
    }

    // the shared shape with one key more, made once and found again by every object after
    fn with_key_attrs(self: &Rc<Self>, key: &str, attrs: u8) -> Rc<Self> {
        if attrs == ATTR_DATA {
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
                attrs: self.attrs.clone(),
                index: self.index.clone(),
                transitions: RefCell::default(),
                attr_transitions: RefCell::default(),
                _parent: Some(self.clone()),
                longest: std::cell::Cell::new(self.keys.len() + 1),
                own: false,
                has_enumerable: self.has_enumerable,
                has_integer_key: self.has_integer_key,
            };
            next.push_own(key.clone(), attrs);
            let next = Rc::new(next);
            transitions.insert(key, Rc::downgrade(&next));
            let length = next.keys.len();
            let mut shape = Some(self);
            while let Some(ancestor) = shape {
                ancestor.longest.set(ancestor.longest.get().max(length));
                shape = ancestor._parent.as_ref();
            }
            return next;
        }
        let mut transitions = self.attr_transitions.borrow_mut();
        for (existing_key, existing_attrs, weak) in transitions.iter() {
            if *existing_attrs == attrs
                && &**existing_key == key
                && let Some(next) = weak.upgrade()
            {
                return next;
            }
        }
        if transitions.len() >= 16 {
            transitions.retain(|(_, _, weak)| weak.strong_count() > 0);
        }
        let key: Rc<str> = Rc::from(key);
        let mut next = Shape {
            keys: self.keys.clone(),
            attrs: self.attrs.clone(),
            index: self.index.clone(),
            transitions: RefCell::default(),
            attr_transitions: RefCell::default(),
            _parent: Some(self.clone()),
            longest: std::cell::Cell::new(self.keys.len() + 1),
            own: false,
            has_enumerable: self.has_enumerable,
            has_integer_key: self.has_integer_key,
        };
        next.push_own(key.clone(), attrs);
        let next = Rc::new(next);
        transitions.push((key, attrs, Rc::downgrade(&next)));
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

const PIC_SIZE: usize = 4;

// per-site 4-entry polymorphic inline cache for GetProp and SetProp; holds shapes weakly so
// their addresses stay unique while referenced
pub(super) struct PropIc(UnsafeCell<PropIcInner>);

struct PropIcInner {
    entries: [IcEntry; PIC_SIZE],
    next: u8,
}

impl Default for PropIc {
    fn default() -> Self {
        Self(UnsafeCell::new(PropIcInner {
            entries: [
                IcEntry::Empty,
                IcEntry::Empty,
                IcEntry::Empty,
                IcEntry::Empty,
            ],
            next: 0,
        }))
    }
}

pub(super) enum IcGetHit {
    Value(Value),
    Getter(Value),
    Missing,
}

struct DeepProtoChain {
    skipped: Vec<(ObjectWeak, Weak<Shape>)>,
    holder: ObjectWeak,
    holder_shape: Weak<Shape>,
    index: u16,
    accessor: bool,
}

struct ProtoGuard {
    holder: ObjectWeak,
    shape: Weak<Shape>,
}

enum IcEntry {
    Empty,
    OwnData {
        shape: Weak<Shape>,
        index: u16,
        writable: bool,
    },
    OwnAccessor {
        shape: Weak<Shape>,
        index: u16,
    },
    OwnAdd {
        from: Weak<Shape>,
        to: Weak<Shape>,
        proto: Option<Box<ProtoGuard>>,
    },
    ProtoData {
        receiver: Option<Weak<Shape>>,
        holder: ObjectWeak,
        holder_shape: Weak<Shape>,
        index: u16,
    },
    ProtoAccessor {
        receiver: Weak<Shape>,
        holder: ObjectWeak,
        holder_shape: Weak<Shape>,
        index: u16,
    },
    DeepProto {
        receiver: Weak<Shape>,
        chain: Box<DeepProtoChain>,
    },
    StringMethod {
        holder: ObjectWeak,
        holder_shape: Weak<Shape>,
        index: u16,
    },
    Missing {
        receiver: Weak<Shape>,
        proto: Option<Box<ProtoGuard>>,
    },
    ArrayLength,
    StringLength,
}

impl PropIc {
    fn replace_or_insert(&self, matches_existing: impl Fn(&IcEntry) -> bool, entry: IcEntry) {
        // safety: single-threaded VM, never re-entered during mutation
        let inner = unsafe { &mut *self.0.get() };
        for slot in &mut inner.entries {
            if matches_existing(slot) {
                *slot = entry;
                return;
            }
        }
        for slot in &mut inner.entries {
            if matches!(slot, IcEntry::Empty) {
                *slot = entry;
                return;
            }
        }
        let slot = inner.next as usize % PIC_SIZE;
        inner.next = inner.next.wrapping_add(1);
        inner.entries[slot] = entry;
    }

    #[inline]
    pub(super) fn is_array_length(&self) -> bool {
        let inner = unsafe { &*self.0.get() };
        inner
            .entries
            .iter()
            .any(|entry| matches!(entry, IcEntry::ArrayLength))
    }

    #[inline]
    pub(super) fn is_string_length(&self) -> bool {
        let inner = unsafe { &*self.0.get() };
        inner
            .entries
            .iter()
            .any(|entry| matches!(entry, IcEntry::StringLength))
    }

    pub(super) fn record_array_length(&self) {
        self.replace_or_insert(
            |entry| matches!(entry, IcEntry::ArrayLength),
            IcEntry::ArrayLength,
        );
    }

    pub(super) fn record_string_length(&self) {
        self.replace_or_insert(
            |entry| matches!(entry, IcEntry::StringLength),
            IcEntry::StringLength,
        );
    }
}

impl std::fmt::Debug for PropIc {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PropIc")
    }
}

// sentinel capacity indicating ptr/len came from Rc<[Slot]>::into_raw rather than an owned Vec
const SHARED_CAP: u32 = u32::MAX;

// compact 16-byte slot storage: either an owned Vec<Slot> buffer or a shared Rc<[Slot]>
// until mutated, keeping JsObject entries compact with branchless reads
struct Properties {
    ptr: std::ptr::NonNull<Slot>,
    len: u32,
    cap: u32,
}

impl Default for Properties {
    fn default() -> Self {
        Self {
            ptr: std::ptr::NonNull::dangling(),
            len: 0,
            cap: 0,
        }
    }
}

impl Drop for Properties {
    fn drop(&mut self) {
        if self.cap == SHARED_CAP {
            let raw =
                std::ptr::slice_from_raw_parts(self.ptr.as_ptr() as *const Slot, self.len as usize);
            // safety: SHARED_CAP entries were created from Rc::<[Slot]>::into_raw
            drop(unsafe { Rc::from_raw(raw) });
        } else if self.cap != 0 {
            // safety: owned entries were created from Vec::<Slot> with matching len and cap
            drop(unsafe {
                Vec::from_raw_parts(self.ptr.as_ptr(), self.len as usize, self.cap as usize)
            });
        }
    }
}

impl Clone for Properties {
    fn clone(&self) -> Self {
        if self.cap == SHARED_CAP {
            let raw =
                std::ptr::slice_from_raw_parts(self.ptr.as_ptr() as *const Slot, self.len as usize);
            // safety: self owns one strong count on raw
            unsafe { Rc::increment_strong_count(raw) };
            Self {
                ptr: self.ptr,
                len: self.len,
                cap: SHARED_CAP,
            }
        } else {
            Self::from_vec(self.to_vec())
        }
    }
}

impl std::ops::Deref for Properties {
    type Target = [Slot];

    #[inline(always)]
    fn deref(&self) -> &[Slot] {
        // safety: ptr is valid and aligned for len initialized Slot elements (or dangling when len == 0)
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len as usize) }
    }
}

impl Properties {
    fn from_vec(vec: Vec<Slot>) -> Self {
        let len = vec.len();
        let cap = vec.capacity();
        assert!(cap < SHARED_CAP as usize, "object property count overflow");
        let mut vec = std::mem::ManuallyDrop::new(vec);
        let ptr = std::ptr::NonNull::new(vec.as_mut_ptr()).expect("vec pointer is non-null");
        Self {
            ptr,
            len: len as u32,
            cap: cap as u32,
        }
    }

    fn from_shared(shared: Rc<[Slot]>) -> Self {
        let len = shared.len();
        assert!(len < SHARED_CAP as usize, "shared property count overflow");
        let raw = Rc::into_raw(shared);
        let ptr = std::ptr::NonNull::new(raw as *mut Slot).expect("rc pointer is non-null");
        Self {
            ptr,
            len: len as u32,
            cap: SHARED_CAP,
        }
    }

    fn make_owned(&mut self) {
        if self.cap == SHARED_CAP {
            let owned = self.to_vec();
            *self = Self::from_vec(owned);
        }
    }

    #[inline]
    fn as_mut_slice(&mut self) -> &mut [Slot] {
        self.make_owned();
        // safety: now uniquely owned with len initialized Slot elements
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len as usize) }
    }

    fn with_vec<R>(&mut self, body: impl FnOnce(&mut Vec<Slot>) -> R) -> R {
        self.make_owned();
        let old = std::mem::take(self);
        let vec = if old.cap == 0 {
            Vec::new()
        } else {
            let old = std::mem::ManuallyDrop::new(old);
            // safety: old was an owned buffer taken out of self
            unsafe { Vec::from_raw_parts(old.ptr.as_ptr(), old.len as usize, old.cap as usize) }
        };
        struct WriteBack<'a> {
            target: &'a mut Properties,
            vec: std::mem::ManuallyDrop<Vec<Slot>>,
        }
        impl Drop for WriteBack<'_> {
            fn drop(&mut self) {
                // safety: drop runs once and moves the Vec back into target
                let vec = unsafe { std::mem::ManuallyDrop::take(&mut self.vec) };
                *self.target = Properties::from_vec(vec);
            }
        }
        let mut guard = WriteBack {
            target: self,
            vec: std::mem::ManuallyDrop::new(vec),
        };
        body(&mut guard.vec)
    }

    fn push(&mut self, slot: Slot) {
        self.with_vec(|vec| vec.push(slot));
    }

    fn reserve_exact(&mut self, additional: usize) {
        self.with_vec(|vec| vec.reserve_exact(additional));
    }

    fn remove(&mut self, index: usize) -> Slot {
        self.with_vec(|vec| vec.remove(index))
    }

    fn share(&mut self) -> Rc<[Slot]> {
        if self.cap == SHARED_CAP {
            let raw =
                std::ptr::slice_from_raw_parts(self.ptr.as_ptr() as *const Slot, self.len as usize);
            // safety: self owns one strong count; bump before handing out a clone
            unsafe {
                Rc::increment_strong_count(raw);
                Rc::from_raw(raw)
            }
        } else {
            let shared: Rc<[Slot]> = Rc::from(&**self);
            *self = Self::from_shared(shared.clone());
            shared
        }
    }

    fn capacity(&self) -> usize {
        if self.cap == SHARED_CAP {
            self.len as usize
        } else {
            self.cap as usize
        }
    }
}

// the keys and values objects made alike start with, which each takes without copying
pub(super) struct ObjectTemplate {
    shape: Rc<Shape>,
    properties: Rc<[Slot]>,
}

impl ObjectTemplate {
    pub(super) fn slots(&self) -> &[Slot] {
        &self.properties
    }
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

const INDEX_THRESHOLD: usize = 8;

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

    // an object built straight from a shared shape and pre-sized slots
    pub(super) fn from_shape(
        shape: Rc<Shape>,
        slots: Vec<Slot>,
        prototype: Option<ObjectHandle>,
    ) -> Self {
        debug_assert_eq!(shape.keys.len(), slots.len());
        Self {
            shape,
            properties: Properties::from_vec(slots),
            prototype,
            orphan: false,
            extensible: true,
            class: ObjectClass::Ordinary,
        }
    }

    // an object with another's keys, prototype and class and these properties in the same
    // order, which shares the other's shape instead of building one key by key
    pub(super) fn with_shape_of(template: &JsObject, properties: Vec<Property>) -> Self {
        debug_assert_eq!(template.properties.len(), properties.len());
        if template.shape.attrs.len() == properties.len()
            && template
                .shape
                .attrs
                .iter()
                .zip(&properties)
                .all(|(&attr, property)| attr == property.attrs())
        {
            return Self {
                shape: template.shape.clone(),
                properties: Properties::from_vec(
                    properties
                        .into_iter()
                        .map(|property| property.slot)
                        .collect(),
                ),
                prototype: template.prototype.clone(),
                orphan: template.orphan,
                extensible: template.extensible,
                class: template.class.clone(),
            };
        }
        let mut object = Self {
            shape: Shape::root(),
            properties: Properties::from_vec(Vec::with_capacity(properties.len())),
            prototype: template.prototype.clone(),
            orphan: template.orphan,
            extensible: template.extensible,
            class: template.class.clone(),
        };
        for (key, property) in template.shape.keys.iter().zip(properties) {
            object.define_key(key, property);
        }
        object
    }

    pub(super) fn shape_handle(&self) -> Option<Rc<Shape>> {
        (!self.shape.own).then(|| self.shape.clone())
    }

    #[inline]
    pub(super) fn ic_get(&self, ic: &PropIc) -> Option<IcGetHit> {
        if matches!(self.class, ObjectClass::Proxy(_)) {
            return None;
        }
        let shape_ptr = Rc::as_ptr(&self.shape);
        // safety: single-threaded VM, entries are read without re-entering PropIc
        let inner = unsafe { &*ic.0.get() };
        for entry in &inner.entries {
            match entry {
                IcEntry::Empty => break,
                IcEntry::OwnData { shape, index, .. } => {
                    if std::ptr::eq(shape.as_ptr(), shape_ptr)
                        && let Some(Slot::Data(value)) = self.properties.get(*index as usize)
                    {
                        return Some(IcGetHit::Value(value.resolve()));
                    }
                }
                IcEntry::OwnAccessor { shape, index } => {
                    if std::ptr::eq(shape.as_ptr(), shape_ptr)
                        && let Some(Slot::Accessor(accessors)) =
                            self.properties.get(*index as usize)
                    {
                        return Some(match &accessors.get {
                            Some(getter) => IcGetHit::Getter(getter.clone()),
                            None => IcGetHit::Value(Value::Undefined),
                        });
                    }
                }
                IcEntry::ProtoData {
                    receiver: Some(receiver),
                    holder,
                    holder_shape,
                    index,
                } => {
                    if std::ptr::eq(receiver.as_ptr(), shape_ptr)
                        && !matches!(self.class, ObjectClass::String(_))
                        && let Some(prototype) = &self.prototype
                        && holder.matches(prototype)
                        && let Some(value) =
                            Self::held_data(prototype, holder_shape, *index as usize)
                    {
                        return Some(IcGetHit::Value(value));
                    }
                }
                IcEntry::ProtoAccessor {
                    receiver,
                    holder,
                    holder_shape,
                    index,
                } => {
                    if std::ptr::eq(receiver.as_ptr(), shape_ptr)
                        && !matches!(self.class, ObjectClass::String(_))
                        && let Some(prototype) = &self.prototype
                        && holder.matches(prototype)
                        && let Some(hit) =
                            Self::held_accessor(prototype, holder_shape, *index as usize)
                    {
                        return Some(hit);
                    }
                }
                IcEntry::DeepProto { receiver, chain } => {
                    if std::ptr::eq(receiver.as_ptr(), shape_ptr)
                        && !matches!(self.class, ObjectClass::String(_))
                        && let Some(prototype) = &self.prototype
                        && let Some(hit) = Self::check_chain(
                            prototype,
                            &chain.skipped,
                            &chain.holder,
                            &chain.holder_shape,
                            chain.index as usize,
                            chain.accessor,
                        )
                    {
                        return Some(hit);
                    }
                }
                IcEntry::Missing { receiver, proto }
                    if std::ptr::eq(receiver.as_ptr(), shape_ptr)
                        && !matches!(self.class, ObjectClass::String(_))
                        && !self.orphan
                        && Self::check_missing_proto(&self.prototype, proto.as_deref()) =>
                {
                    return Some(IcGetHit::Missing);
                }
                _ => {}
            }
        }
        None
    }

    // writes an ordinary object's own writable data property, or transitions to a cached shape
    // when adding a new property on an extensible object with a guarded root prototype
    #[inline]
    pub(super) fn ic_set(&mut self, ic: &PropIc, value: Value) -> Result<(), Value> {
        if !matches!(self.class, ObjectClass::Ordinary) {
            return Err(value);
        }
        let shape_ptr = Rc::as_ptr(&self.shape);
        // safety: single-threaded VM, entries are read without re-entering PropIc
        let inner = unsafe { &*ic.0.get() };
        for entry in &inner.entries {
            match entry {
                IcEntry::Empty => break,
                IcEntry::OwnData {
                    shape,
                    index,
                    writable: true,
                } => {
                    if std::ptr::eq(shape.as_ptr(), shape_ptr) {
                        let index = *index as usize;
                        let plain = matches!(
                            self.properties.get(index),
                            Some(Slot::Data(current))
                                if !matches!(current, Value::Binding(_) | Value::ConstBinding(_))
                        );
                        if !plain {
                            return Err(value);
                        }
                        if let Slot::Data(slot) = &mut self.properties.as_mut_slice()[index] {
                            let old = std::mem::replace(slot, value);
                            old.fast_drop();
                        }
                        return Ok(());
                    }
                }
                IcEntry::OwnAdd { from, to, proto } => {
                    if std::ptr::eq(from.as_ptr(), shape_ptr)
                        && self.extensible
                        && !self.orphan
                        && Self::check_missing_proto(&self.prototype, proto.as_deref())
                        && let Some(next_shape) = to.upgrade()
                    {
                        let length = self.properties.len();
                        self.shape = next_shape;
                        if length == self.properties.capacity() {
                            let doubled = (length * 2).max(4);
                            let wanted = self.shape.longest.get().clamp(length + 1, doubled);
                            self.properties.reserve_exact(wanted - length);
                        }
                        self.properties.push(Slot::Data(value));
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
        Err(value)
    }

    #[inline]
    pub(super) fn ic_array_get(prototype: &ObjectHandle, ic: &PropIc) -> Option<Value> {
        let inner = unsafe { &*ic.0.get() };
        for entry in &inner.entries {
            match entry {
                IcEntry::Empty => break,
                IcEntry::ProtoData {
                    receiver: None,
                    holder,
                    holder_shape,
                    index,
                } if holder.matches(prototype) => {
                    if let Some(value) = Self::held_data(prototype, holder_shape, *index as usize) {
                        return Some(value);
                    }
                }
                _ => {}
            }
        }
        None
    }

    #[inline]
    pub(super) fn ic_string_get(prototype: &ObjectHandle, ic: &PropIc) -> Option<Value> {
        let inner = unsafe { &*ic.0.get() };
        for entry in &inner.entries {
            match entry {
                IcEntry::Empty => break,
                IcEntry::StringMethod {
                    holder,
                    holder_shape,
                    index,
                } if holder.matches(prototype) => {
                    if let Some(value) = Self::held_data(prototype, holder_shape, *index as usize) {
                        return Some(value);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn check_missing_proto(prototype: &Option<ObjectHandle>, guard: Option<&ProtoGuard>) -> bool {
        match (prototype, guard) {
            (None, None) => true,
            (Some(proto), Some(guard)) => {
                if !guard.holder.matches(proto) {
                    return false;
                }
                let Ok(borrowed) = proto.try_borrow() else {
                    return false;
                };
                borrowed.prototype.is_none()
                    && !borrowed.orphan
                    && !matches!(
                        borrowed.class,
                        ObjectClass::Proxy(_) | ObjectClass::String(_)
                    )
                    && std::ptr::eq(Rc::as_ptr(&borrowed.shape), guard.shape.as_ptr())
            }
            _ => false,
        }
    }

    fn held_data(holder: &ObjectHandle, shape: &Weak<Shape>, index: usize) -> Option<Value> {
        let holder = holder.try_borrow().ok()?;
        if matches!(holder.class, ObjectClass::Proxy(_))
            || !std::ptr::eq(shape.as_ptr(), Rc::as_ptr(&holder.shape))
        {
            return None;
        }
        holder.properties.get(index)?.value().map(Value::resolve)
    }

    fn held_accessor(holder: &ObjectHandle, shape: &Weak<Shape>, index: usize) -> Option<IcGetHit> {
        let holder = holder.try_borrow().ok()?;
        if matches!(holder.class, ObjectClass::Proxy(_))
            || !std::ptr::eq(shape.as_ptr(), Rc::as_ptr(&holder.shape))
        {
            return None;
        }
        match holder.properties.get(index)? {
            Slot::Accessor(accessors) => Some(match &accessors.get {
                Some(getter) => IcGetHit::Getter(getter.clone()),
                None => IcGetHit::Value(Value::Undefined),
            }),
            Slot::Data(_) => None,
        }
    }

    fn check_chain(
        current: &ObjectHandle,
        skipped: &[(ObjectWeak, Weak<Shape>)],
        holder: &ObjectWeak,
        holder_shape: &Weak<Shape>,
        index: usize,
        accessor: bool,
    ) -> Option<IcGetHit> {
        if let Some(((expected, shape), rest)) = skipped.split_first() {
            if !expected.matches(current) {
                return None;
            }
            let borrowed = current.try_borrow().ok()?;
            if matches!(
                borrowed.class,
                ObjectClass::Proxy(_) | ObjectClass::String(_)
            ) || !std::ptr::eq(shape.as_ptr(), Rc::as_ptr(&borrowed.shape))
            {
                return None;
            }
            let next = borrowed.prototype.as_ref()?;
            Self::check_chain(next, rest, holder, holder_shape, index, accessor)
        } else {
            if !holder.matches(current) {
                return None;
            }
            if accessor {
                Self::held_accessor(current, holder_shape, index)
            } else {
                Self::held_data(current, holder_shape, index).map(IcGetHit::Value)
            }
        }
    }

    pub(super) fn record_get_ic(&self, key: &str, ic: &PropIc) {
        if self.shape.own || matches!(self.class, ObjectClass::Proxy(_)) {
            return;
        }
        let shape_ptr = Rc::as_ptr(&self.shape);
        if let Some(index) = self.shape.position(key) {
            let Ok(index_u16) = u16::try_from(index) else {
                return;
            };
            let attrs = self.shape.attrs[index];
            let entry = if (attrs & ATTR_ACCESSOR) != 0 {
                IcEntry::OwnAccessor {
                    shape: Rc::downgrade(&self.shape),
                    index: index_u16,
                }
            } else {
                IcEntry::OwnData {
                    shape: Rc::downgrade(&self.shape),
                    index: index_u16,
                    writable: (attrs & ATTR_WRITABLE) != 0,
                }
            };
            ic.replace_or_insert(
                |existing| match existing {
                    IcEntry::OwnData { shape, .. } | IcEntry::OwnAccessor { shape, .. } => {
                        std::ptr::eq(shape.as_ptr(), shape_ptr)
                    }
                    _ => false,
                },
                entry,
            );
            return;
        }
        if matches!(self.class, ObjectClass::String(_)) {
            return;
        }
        let Some(first_proto) = self.prototype.clone() else {
            if !self.orphan {
                ic.replace_or_insert(
                    |existing| {
                        matches!(existing, IcEntry::Missing { receiver, .. } if std::ptr::eq(receiver.as_ptr(), shape_ptr))
                    },
                    IcEntry::Missing {
                        receiver: Rc::downgrade(&self.shape),
                        proto: None,
                    },
                );
            }
            return;
        };
        // walk up to 6 prototype hops (covers HTML* -> Element -> Node -> EventTarget -> Object)
        let mut skipped: Vec<(ObjectWeak, Weak<Shape>)> = Vec::new();
        let mut current = first_proto;
        for depth in 0..6 {
            let Ok(object) = current.try_borrow() else {
                return;
            };
            if object.shape.own
                || matches!(object.class, ObjectClass::Proxy(_) | ObjectClass::String(_))
            {
                return;
            }
            if let Some(index) = object.shape.position(key) {
                let Ok(index_u16) = u16::try_from(index) else {
                    return;
                };
                let accessor = (object.shape.attrs[index] & ATTR_ACCESSOR) != 0;
                let entry = if depth == 0 {
                    if accessor {
                        IcEntry::ProtoAccessor {
                            receiver: Rc::downgrade(&self.shape),
                            holder: current.downgrade(),
                            holder_shape: Rc::downgrade(&object.shape),
                            index: index_u16,
                        }
                    } else {
                        IcEntry::ProtoData {
                            receiver: Some(Rc::downgrade(&self.shape)),
                            holder: current.downgrade(),
                            holder_shape: Rc::downgrade(&object.shape),
                            index: index_u16,
                        }
                    }
                } else {
                    IcEntry::DeepProto {
                        receiver: Rc::downgrade(&self.shape),
                        chain: Box::new(DeepProtoChain {
                            skipped,
                            holder: current.downgrade(),
                            holder_shape: Rc::downgrade(&object.shape),
                            index: index_u16,
                            accessor,
                        }),
                    }
                };
                ic.replace_or_insert(
                    |existing| match existing {
                        IcEntry::ProtoData {
                            receiver: Some(receiver),
                            ..
                        }
                        | IcEntry::ProtoAccessor { receiver, .. }
                        | IcEntry::DeepProto { receiver, .. }
                        | IcEntry::Missing { receiver, .. } => {
                            std::ptr::eq(receiver.as_ptr(), shape_ptr)
                        }
                        _ => false,
                    },
                    entry,
                );
                return;
            }
            if object.prototype.is_none() {
                if depth == 0 && !object.orphan && !self.orphan {
                    ic.replace_or_insert(
                        |existing| {
                            matches!(existing, IcEntry::Missing { receiver, .. } if std::ptr::eq(receiver.as_ptr(), shape_ptr))
                        },
                        IcEntry::Missing {
                            receiver: Rc::downgrade(&self.shape),
                            proto: Some(Box::new(ProtoGuard {
                                holder: current.downgrade(),
                                shape: Rc::downgrade(&object.shape),
                            })),
                        },
                    );
                }
                return;
            }
            let next = object.prototype.clone().expect("checked above");
            skipped.push((current.downgrade(), Rc::downgrade(&object.shape)));
            drop(object);
            current = next;
        }
    }

    pub(super) fn record_set_ic(&self, key: &str, from_shape: Option<Rc<Shape>>, ic: &PropIc) {
        if self.shape.own || !matches!(self.class, ObjectClass::Ordinary) {
            return;
        }
        let Some(index) = self.shape.position(key) else {
            return;
        };
        let Ok(index_u16) = u16::try_from(index) else {
            return;
        };
        let attrs = self.shape.attrs[index];
        if (attrs & ATTR_ACCESSOR) != 0 || (attrs & ATTR_WRITABLE) == 0 {
            return;
        }
        if let Some(from) = from_shape
            && !from.own
            && !Rc::ptr_eq(&from, &self.shape)
            && self.shape.keys.len() == from.keys.len() + 1
            && index == from.keys.len()
            && attrs == ATTR_DATA
            && !self.orphan
        {
            let proto_guard = match &self.prototype {
                None => Some(None),
                Some(proto) => proto.try_borrow().ok().and_then(|borrowed| {
                    (borrowed.prototype.is_none()
                        && !borrowed.orphan
                        && !borrowed.shape.own
                        && !matches!(
                            borrowed.class,
                            ObjectClass::Proxy(_) | ObjectClass::String(_)
                        )
                        && !borrowed.contains_key(key))
                    .then(|| {
                        Some(Box::new(ProtoGuard {
                            holder: proto.downgrade(),
                            shape: Rc::downgrade(&borrowed.shape),
                        }))
                    })
                }),
            };
            if let Some(proto) = proto_guard {
                let from_ptr = Rc::as_ptr(&from);
                ic.replace_or_insert(
                    |existing| {
                        matches!(existing, IcEntry::OwnAdd { from, .. } if std::ptr::eq(from.as_ptr(), from_ptr))
                    },
                    IcEntry::OwnAdd {
                        from: Rc::downgrade(&from),
                        to: Rc::downgrade(&self.shape),
                        proto,
                    },
                );
                return;
            }
        }
        let shape_ptr = Rc::as_ptr(&self.shape);
        ic.replace_or_insert(
            |existing| {
                matches!(existing, IcEntry::OwnData { shape, .. } if std::ptr::eq(shape.as_ptr(), shape_ptr))
            },
            IcEntry::OwnData {
                shape: Rc::downgrade(&self.shape),
                index: index_u16,
                writable: true,
            },
        );
    }

    pub(super) fn record_array_method_ic(prototype: &ObjectHandle, key: &str, ic: &PropIc) {
        let Ok(object) = prototype.try_borrow() else {
            return;
        };
        if object.shape.own || matches!(object.class, ObjectClass::Proxy(_)) {
            return;
        }
        if let Some(index) = object.shape.position(key)
            && (object.shape.attrs[index] & ATTR_ACCESSOR) == 0
            && let Ok(index_u16) = u16::try_from(index)
        {
            ic.replace_or_insert(
                |existing| matches!(existing, IcEntry::ProtoData { receiver: None, .. }),
                IcEntry::ProtoData {
                    receiver: None,
                    holder: prototype.downgrade(),
                    holder_shape: Rc::downgrade(&object.shape),
                    index: index_u16,
                },
            );
        }
    }

    pub(super) fn record_string_method_ic(prototype: &ObjectHandle, key: &str, ic: &PropIc) {
        let Ok(object) = prototype.try_borrow() else {
            return;
        };
        if object.shape.own || matches!(object.class, ObjectClass::Proxy(_)) {
            return;
        }
        if let Some(index) = object.shape.position(key)
            && (object.shape.attrs[index] & ATTR_ACCESSOR) == 0
            && let Ok(index_u16) = u16::try_from(index)
        {
            ic.replace_or_insert(
                |existing| matches!(existing, IcEntry::StringMethod { .. }),
                IcEntry::StringMethod {
                    holder: prototype.downgrade(),
                    holder_shape: Rc::downgrade(&object.shape),
                    index: index_u16,
                },
            );
        }
    }

    // the keys and values this object has, for others made alike to start from; it then holds
    // its values in common with them, and an object with a shape of its own makes none
    pub(super) fn template(&mut self) -> Option<ObjectTemplate> {
        if self.shape.own {
            return None;
        }
        let shared = self.properties.share();
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
            properties: Properties::from_shared(template.properties.clone()),
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
        self.properties.reserve_exact(properties);
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

    #[inline]
    pub(super) fn property(&self, key: &str) -> Option<PropertyRef<'_>> {
        let position = self.position(key)?;
        let attrs = self.shape.attrs[position];
        Some(PropertyRef {
            slot: &self.properties[position],
            enumerable: (attrs & ATTR_ENUMERABLE) != 0,
            writable: (attrs & ATTR_WRITABLE) != 0,
            configurable: (attrs & ATTR_CONFIGURABLE) != 0,
        })
    }

    pub(super) fn slot_mut(&mut self, key: &str) -> Option<&mut Slot> {
        self.position(key)
            .map(move |position| &mut self.properties.as_mut_slice()[position])
    }

    // defines or replaces a property with exact attributes
    pub(super) fn define(&mut self, key: String, property: Property) {
        self.define_key(&key, property);
    }

    // the same with a borrowed key, which a shared shape that already has it never copies
    pub(super) fn define_key(&mut self, key: &str, property: Property) {
        let attrs = property.attrs();
        if let Some(position) = self.position(key) {
            if self.shape.attrs[position] != attrs {
                let shape = self.own_shape();
                shape.attrs[position] = attrs;
                shape.recompute_flags();
            }
            self.properties.as_mut_slice()[position] = property.slot;
            return;
        }
        if self.shape.own || self.properties.len() >= SHARED_KEYS {
            self.own_shape().push_own(Rc::from(key), attrs);
        } else {
            self.shape = self.shape.with_key_attrs(key, attrs);
            let length = self.properties.len();
            if length == self.properties.capacity() {
                // never more than doubling would take, so a shape shared by unlike objects only
                // ever saves room
                let doubled = (length * 2).max(4);
                let wanted = self.shape.longest.get().clamp(length + 1, doubled);
                self.properties.reserve_exact(wanted - length);
            }
        }
        self.properties.push(property.slot);
    }

    // the data value of an own property; accessors read as absent for host code
    pub(super) fn get(&self, key: &str) -> Option<&Value> {
        let position = self.position(key)?;
        self.properties[position].value()
    }

    pub(super) fn contains_key(&self, key: &str) -> bool {
        self.position(key).is_some()
    }

    // sets a data value, keeping the attributes of an existing property
    pub(super) fn insert(&mut self, key: String, value: Value) -> Option<Value> {
        if let Some(position) = self.position(&key) {
            if (self.shape.attrs[position] & ATTR_ACCESSOR) != 0 {
                let shape = self.own_shape();
                shape.attrs[position] &= !ATTR_ACCESSOR;
            }
            let slot = &mut self.properties.as_mut_slice()[position];
            let previous = slot.value().cloned();
            *slot = Slot::Data(value);
            return previous;
        }
        self.define(key, Property::data(value));
        None
    }

    pub(super) fn remove(&mut self, key: &str) -> Option<Value> {
        let position = self.position(key)?;
        let slot = self.properties.remove(position);
        let shape = self.own_shape();
        shape.keys.remove(position);
        shape.attrs.remove(position);
        shape.reindex();
        shape.recompute_flags();
        slot.value().cloned()
    }

    // own property keys in spec order: integer keys ascending, then strings in insertion order
    pub(super) fn ordered_keys(&self) -> Vec<String> {
        if !self.shape.has_integer_key {
            return self.shape.keys.iter().map(|key| key.to_string()).collect();
        }
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

    pub(super) fn has_enumerable(&self) -> bool {
        self.shape.has_enumerable
    }

    // collects own enumerable keys in spec order directly from shape without re-looking up positions
    pub(super) fn collect_enumerable_keys(&self, include_symbols: bool, out: &mut Vec<String>) {
        if !include_symbols && !self.shape.has_enumerable {
            return;
        }
        if !self.shape.has_integer_key {
            for (key, &attrs) in self.shape.keys.iter().zip(&self.shape.attrs) {
                if (attrs & ATTR_ENUMERABLE) == 0 || is_private_key(key) {
                    continue;
                }
                if !include_symbols && is_symbol_key(key) {
                    continue;
                }
                out.push(key.to_string());
            }
            return;
        }
        let mut integers = Vec::new();
        let mut strings = Vec::new();
        for (key, &attrs) in self.shape.keys.iter().zip(&self.shape.attrs) {
            if (attrs & ATTR_ENUMERABLE) == 0 || is_private_key(key) {
                continue;
            }
            if !include_symbols && is_symbol_key(key) {
                continue;
            }
            match array_index(key) {
                Some(index) => integers.push((index, key.to_string())),
                None => strings.push(key.to_string()),
            }
        }
        integers.sort_unstable_by_key(|(index, _)| *index);
        out.extend(integers.into_iter().map(|(_, key)| key));
        out.extend(strings);
    }

    // visits own non-symbol non-private keys with their enumerable bit in spec order
    pub(super) fn visit_for_in_keys(&self, mut visit: impl FnMut(&str, bool)) {
        if !self.shape.has_integer_key {
            for (key, &attrs) in self.shape.keys.iter().zip(&self.shape.attrs) {
                if is_symbol_key(key) || is_private_key(key) {
                    continue;
                }
                visit(key, (attrs & ATTR_ENUMERABLE) != 0);
            }
            return;
        }
        let mut integers = Vec::new();
        let mut strings = Vec::new();
        for (key, &attrs) in self.shape.keys.iter().zip(&self.shape.attrs) {
            if is_symbol_key(key) || is_private_key(key) {
                continue;
            }
            let enumerable = (attrs & ATTR_ENUMERABLE) != 0;
            match array_index(key) {
                Some(index) => integers.push((index, &**key, enumerable)),
                None => strings.push((&**key, enumerable)),
            }
        }
        integers.sort_unstable_by_key(|(index, _, _)| *index);
        for (_, key, enumerable) in integers {
            visit(key, enumerable);
        }
        for (key, enumerable) in strings {
            visit(key, enumerable);
        }
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

    // the own slots in insertion order, without their keys
    pub(super) fn slots(&self) -> &[Slot] {
        &self.properties
    }

    pub(super) fn attrs(&self) -> &[u8] {
        &self.shape.attrs
    }

    // the prototype set on the object itself, which the cycle collector follows
    pub(super) fn own_prototype(&self) -> Option<&ObjectHandle> {
        self.prototype.as_ref()
    }

    pub(super) fn freeze(&mut self, writable: bool) {
        self.extensible = false;
        if !self.shape.attrs.is_empty() {
            let shape = self.own_shape();
            for attr in &mut shape.attrs {
                *attr &= !ATTR_CONFIGURABLE;
                if !writable && (*attr & ATTR_ACCESSOR) == 0 {
                    *attr &= !ATTR_WRITABLE;
                }
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

#[inline]
pub(super) fn new_object(object: JsObject) -> ObjectHandle {
    super::heap::alloc_object(object)
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
        assert_eq!(std::mem::size_of::<Slot>(), 16);
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

#[cfg(test)]
#[test]
fn an_object_fits_the_allocator_class_below_its_old_one() {
    assert_eq!(std::mem::size_of::<ObjectClass>(), 16);
    assert_eq!(std::mem::size_of::<JsObject>(), 56);
}
