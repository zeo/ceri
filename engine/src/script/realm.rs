// the intrinsic objects every script sees (Object, Function, Symbol, the Error family,
// Boolean, Number, String, Array and the iterator and generator prototypes) and the native
// functions behind their methods

use super::ast::{EvalContext, FunctionKind};
use super::compiler::{BindingKind, CallerScope, Code, Compiler, EvalCode, ScopeRole};
use super::object::{
    ArrayIteration, ArrayIterationKind, JsObject, ObjectClass, ObjectHandle, Property,
    SYMBOL_PREFIX, Slot, array_index, is_private_key, is_symbol_key, new_object,
};
use super::operations::{
    is_callable, is_primitive, number_to_string, same_value, symbol_description,
};
use super::vm::{AsyncResumeHandle, ResumeMode, Scope, ScopeRef};
use super::*;

#[derive(Clone)]
pub(super) enum Native {
    ObjectConstructor,
    ObjectCreate,
    ObjectDefineProperty,
    ObjectDefineProperties,
    ObjectGetPrototypeOf,
    ObjectSetPrototypeOf,
    ObjectGetOwnPropertyNames,
    ObjectGetOwnPropertySymbols,
    ObjectGetOwnPropertyDescriptor,
    ObjectGetOwnPropertyDescriptors,
    ObjectKeys,
    ObjectValues,
    ObjectEntries,
    ObjectAssign,
    ObjectFromEntries,
    ObjectIs,
    ObjectFreeze,
    ObjectIsFrozen,
    ObjectSeal,
    ObjectIsSealed,
    ObjectPreventExtensions,
    ObjectIsExtensible,
    ObjectHasOwn,
    ObjectHasOwnProperty,
    ObjectIsPrototypeOf,
    ObjectPropertyIsEnumerable,
    ObjectToString,
    ObjectToLocaleString,
    ObjectValueOf,
    ObjectProtoGetter,
    ObjectProtoSetter,
    // the legacy accessor helpers, from the annex that adds them to the prototype
    ObjectDefineGetter,
    ObjectDefineSetter,
    ObjectLookupGetter,
    ObjectLookupSetter,
    FunctionConstructor,
    FunctionPrototype,
    FunctionCall,
    FunctionApply,
    FunctionBind,
    FunctionToString,
    FunctionHasInstance,
    SymbolConstructor,
    SymbolFor,
    SymbolKeyFor,
    SymbolToString,
    SymbolValueOf,
    SymbolDescription,
    ErrorConstructor(&'static str),
    ErrorToString,
    ErrorCaptureStackTrace,
    ErrorIsError,
    RegExpEscape,
    BooleanConstructor,
    BooleanToString,
    BooleanValueOf,
    NumberConstructor,
    NumberToString,
    NumberValueOf,
    NumberIsInteger,
    NumberIsSafeInteger,
    NumberIsNaN,
    StringConstructor,
    StringToString,
    StringFromCharCode,
    ArrayConstructor,
    ArrayFrom,
    ArrayOf,
    ArrayValues,
    ArrayIteratorNext,
    ArrayKeys,
    ArrayEntries,
    IteratorSelf,
    GeneratorNext,
    GeneratorReturn,
    GeneratorThrow,
    AsyncGeneratorNext,
    AsyncGeneratorReturn,
    AsyncGeneratorThrow,
    AsyncResume(AsyncResumeHandle, bool),
    IterResultWrap(bool),
    Array(super::arrays::ArrayMethod),
    String(super::strings::StringMethod),
    Math(super::numbers::MathFunction),
    NumberFormat(super::numbers::NumberFormat),
    RegExp(super::regexp::RegExpMethod),
    Console(super::console::ConsoleMethod),
    Promise(Rc<super::promises::PromiseFunction>),
    ParseInt,
    ParseFloat,
    Eval,
    // true for the component forms
    EncodeUri(bool),
    CssEscape,
    CssSupports,
    DecodeUri(bool),
    GlobalIsNaN,
    GlobalIsFinite,
    StringRaw,
    StringFromCodePoint,
    JsonParse,
    JsonRawJson,
    JsonIsRawJson,
    JsonStringify,
    Reflect(super::reflect::ReflectMethod),
    ProxyRevoke(Rc<super::reflect::ProxyData>),
    Collection(super::collections::CollectionMethod),
    Date(super::dates::DateMethod),
    Typed(super::typed::TypedMethod),
    // ascending numeric order, the default sort of typed arrays
    NumericCompare,
    Codec(super::encoding::CodecMethod),
    BigIntCall,
    BigIntAsN(bool),
    BigIntToString,
    BigIntValueOf,
    Intl(Rc<super::intl::IntlMethod>),
    Dom(super::interfaces::DomMember),
    DomConstructor(&'static str),
    Event(super::events::EventMember),
    EventConstructor(&'static str),
    Range(super::ranges::RangeMember),
    RangeConstructor(&'static str),
    Window(super::geometry::WindowMethod),
    Observer(super::observers::ObserverMethod),
    ObserverConstructor(super::observers::ObserverKind),
    Xhr(super::xhr::XhrMember),
    XhrConstructor,
    Socket(super::sockets::SocketMember),
    SocketConstructor,
    Source(super::eventsource::SourceMember),
    SourceConstructor,
    // an event source that lost its connection tries again
    SourceReconnect(u32),
    Notice(super::notifications::NoticeMember),
    NoticeConstructor,
    Channel(super::broadcast::ChannelMember),
    ChannelConstructor,
    // the messages posted on broadcast channels since the last task go out
    ChannelDeliver,
    Reader(super::filereader::ReaderMember),
    ReaderConstructor,
    // a reader's read of the given generation completes
    ReaderDeliver(u32, u64),
    Port(super::messaging::PortMember),
    Walk(super::traversal::WalkMember),
    Helper(super::iterator_helpers::HelperMember),
    Markup(super::markup::MarkupMember),
    Path(super::path2d::PathMember),
    Matrix(super::matrices::MatrixMember),
    Segmenter(super::segmenter::SegmenterMember),
    Disposable(super::disposables::DisposableMember, bool),
    Atomics(super::atomics::AtomicsOp),
    ViewTransition(
        Rc<super::viewtransitions::ViewTransition>,
        super::viewtransitions::TransitionStep,
    ),
    Cssom(super::cssom::CssomMember),
    Font(super::fontfaces::FontMember),
    // DOMMatrixReadOnly when true, DOMMatrix otherwise
    MatrixConstructor(bool),
    Stream(Rc<super::streams::StreamFunction>),
    Nav(Rc<super::navigator_apis::NavFunction>),
    Anim(super::animations::AnimMember),
    AnimConstructor,
    ClipboardItemConstructor,
    DomExceptionConstructor,
    // reports a value as an uncaught exception would be, without throwing
    ReportError,
    // the signal of the outside fetch with this id was aborted
    FetchAbort(u32),
    // an animation of the given generation reaches its end
    AnimFinish(u32, u64),
    Shape(super::geometry_values::ShapeMember),
    // DOMRect or DOMRectReadOnly, and DOMPoint or DOMPointReadOnly, by whether read only
    RectConstructor(bool),
    PointConstructor(bool),
    PortConstructor,
    // an interface whose objects only the engine makes
    IllegalConstructor,
    QuadConstructor,
    Locale(super::locale::LocaleMember),
    DisplayNames(super::displaynames::DisplayMember),
    DurationFormat(super::durationformat::DurationMember),
    Offscreen(super::offscreen::OffscreenMember),
    UrlPattern(super::urlpattern::PatternMember),
    XPath(super::xpath::XPathMember),
    StyleOm(super::typed_om::StyleMember),
    Temporal(super::temporal::TemporalMember),
    PerfObserver(super::perf_observers::ObserverMember),
    PerfDeliver(Rc<super::perf_observers::PerfObserver>),
    DataTransferConstructor,
    MessageChannelConstructor,
    // the messages posted to ports and the window since the last task arrive
    PortDeliver,
    Url(super::urls::UrlMember),
    UrlConstructor,
    UrlSearchParams(super::urls::ParamsMember),
    UrlSearchParamsConstructor,
    // an asynchronous XMLHttpRequest reporting its answer
    XhrDeliver(u32, u64),
    // an img reporting the outcome of the request it started in this generation
    ImageDeliver(crate::dom::NodeId, u64),
    // an inserted script element's load and run, as a task
    ScriptElement(crate::dom::NodeId),
    // a dialog reporting that it closed
    DialogClose(crate::dom::NodeId),
    // a media element's source selection and its events, for one run of its load algorithm
    MediaLoad(crate::dom::NodeId, u64),
    MediaCue(Box<(crate::dom::NodeId, u64, super::media::Cue)>),
    CustomElements(super::custom::RegistryMethod),
    Audio(super::audio::AudioMember),
    AudioConstructor(super::audio::AudioClass),
    // an audio context announcing that it resumed, suspended or closed
    AudioStateChange(u32),
}

impl Native {
    fn is_constructor(&self) -> bool {
        matches!(
            self,
            Native::ObjectConstructor
                | Native::FunctionConstructor
                | Native::ErrorConstructor(_)
                | Native::BooleanConstructor
                | Native::NumberConstructor
                | Native::StringConstructor
                | Native::ArrayConstructor
                | Native::RegExp(super::regexp::RegExpMethod::Constructor)
                | Native::Reflect(super::reflect::ReflectMethod::ProxyConstructor)
                | Native::Collection(super::collections::CollectionMethod::Constructor(_))
                | Native::Collection(super::collections::CollectionMethod::WeakRefConstructor)
                | Native::Collection(super::collections::CollectionMethod::RegistryConstructor)
                | Native::Date(super::dates::DateMethod::Constructor)
                | Native::Typed(super::typed::TypedMethod::Constructor(_))
                | Native::Typed(super::typed::TypedMethod::BufferConstructor)
                | Native::Typed(super::typed::TypedMethod::ViewConstructor)
                | Native::Codec(super::encoding::CodecMethod::EncoderConstructor)
                | Native::Codec(super::encoding::CodecMethod::DecoderConstructor)
                | Native::DomConstructor(_)
                | Native::EventConstructor(_)
                | Native::RangeConstructor(_)
                | Native::ObserverConstructor(_)
                | Native::XhrConstructor
                | Native::SocketConstructor
                | Native::SourceConstructor
                | Native::NoticeConstructor
                | Native::ChannelConstructor
                | Native::ReaderConstructor
                | Native::AnimConstructor
                | Native::ClipboardItemConstructor
                | Native::DomExceptionConstructor
                | Native::Path(super::path2d::PathMember::Constructor)
                | Native::MatrixConstructor(_)
                | Native::Segmenter(super::segmenter::SegmenterMember::Constructor)
                | Native::Disposable(super::disposables::DisposableMember::Constructor, _)
                | Native::Cssom(super::cssom::CssomMember::SheetConstructor)
                | Native::Cssom(super::cssom::CssomMember::RuleConstructor)
                | Native::Font(super::fontfaces::FontMember::FaceConstructor)
                | Native::Font(super::fontfaces::FontMember::SetConstructor)
                | Native::Helper(super::iterator_helpers::HelperMember::Constructor)
                | Native::Markup(
                    super::markup::MarkupMember::Parser | super::markup::MarkupMember::Serializer
                )
                | Native::RectConstructor(_)
                | Native::PointConstructor(_)
                | Native::PortConstructor
                | Native::IllegalConstructor
                | Native::QuadConstructor
                | Native::Locale(super::locale::LocaleMember::Constructor)
                | Native::DisplayNames(super::displaynames::DisplayMember::Constructor)
                | Native::DurationFormat(super::durationformat::DurationMember::Constructor)
                | Native::Offscreen(super::offscreen::OffscreenMember::Constructor)
                | Native::UrlPattern(super::urlpattern::PatternMember::Constructor)
                | Native::XPath(super::xpath::XPathMember::Constructor)
                | Native::StyleOm(super::typed_om::StyleMember::Construct(_))
                | Native::Temporal(super::temporal::TemporalMember::Construct(_))
                | Native::PerfObserver(
                    super::perf_observers::ObserverMember::Constructor
                        | super::perf_observers::ObserverMember::ReportingConstructor
                )
                | Native::DataTransferConstructor
                | Native::MessageChannelConstructor
                | Native::UrlConstructor
                | Native::UrlSearchParamsConstructor
                | Native::AudioConstructor(_)
        ) || matches!(self, Native::Intl(method) if matches!(**method,
            super::intl::IntlMethod::NumberFormatConstructor
                | super::intl::IntlMethod::DateTimeFormatConstructor
                | super::intl::IntlMethod::CollatorConstructor
                | super::intl::IntlMethod::PluralRulesConstructor
                | super::intl::IntlMethod::RelativeTimeFormatConstructor
                | super::intl::IntlMethod::ListFormatConstructor))
            || matches!(self, Native::Promise(function)
                if matches!(**function, super::promises::PromiseFunction::Constructor))
            || matches!(self, Native::Stream(function) if function.is_constructor())
    }
}

impl fmt::Debug for Native {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Native")
    }
}

// interfaces whose objects the engine hands out, so instanceof and their toString tags answer
// as in browsers
const HOST_INTERFACES: &[&str] = &[
    "History",
    "Navigator",
    "Performance",
    "MediaQueryList",
    "FileList",
    "TextMetrics",
    "IDBFactory",
    "IdleDeadline",
    "Lock",
    "LockManager",
    "CanvasRenderingContext2D",
    "ImageBitmap",
    "VisualViewport",
    "ViewTransition",
];

pub(super) struct Intrinsics {
    pub(super) object_prototype: ObjectHandle,
    pub(super) function_prototype: ObjectHandle,
    pub(super) async_function_prototype: ObjectHandle,
    pub(super) generator_function_prototype: ObjectHandle,
    pub(super) async_generator_function_prototype: ObjectHandle,
    pub(super) generator_prototype: ObjectHandle,
    pub(super) async_generator_prototype: ObjectHandle,
    pub(super) array_prototype: ObjectHandle,
    pub(super) string_prototype: ObjectHandle,
    pub(super) number_prototype: ObjectHandle,
    pub(super) boolean_prototype: ObjectHandle,
    pub(super) symbol_prototype: ObjectHandle,
    pub(super) error_prototype: ObjectHandle,
    pub(super) type_error_prototype: ObjectHandle,
    pub(super) range_error_prototype: ObjectHandle,
    pub(super) syntax_error_prototype: ObjectHandle,
    pub(super) reference_error_prototype: ObjectHandle,
    pub(super) eval_error_prototype: ObjectHandle,
    pub(super) uri_error_prototype: ObjectHandle,
    pub(super) aggregate_error_prototype: ObjectHandle,
    pub(super) suppressed_error_prototype: ObjectHandle,
    pub(super) dom_exception_prototype: ObjectHandle,
    pub(super) promise_prototype: ObjectHandle,
    pub(super) promise_constructor: ObjectHandle,
    pub(super) regexp_prototype: ObjectHandle,
    pub(super) array_values: Value,
    pub(super) iterator_prototype: ObjectHandle,
    pub(super) array_iterator_prototype: ObjectHandle,
    pub(super) map_prototype: ObjectHandle,
    pub(super) set_prototype: ObjectHandle,
    pub(super) weak_map_prototype: ObjectHandle,
    pub(super) weak_set_prototype: ObjectHandle,
    pub(super) weak_ref_prototype: ObjectHandle,
    pub(super) registry_prototype: ObjectHandle,
    pub(super) collection_iterator_prototype: ObjectHandle,
    pub(super) date_prototype: ObjectHandle,
    pub(super) typed_prototypes: Vec<ObjectHandle>,
    pub(super) array_buffer_prototype: ObjectHandle,
    pub(super) data_view_prototype: ObjectHandle,
    pub(super) text_encoder_prototype: ObjectHandle,
    pub(super) bigint_prototype: ObjectHandle,
    pub(super) text_decoder_prototype: ObjectHandle,
    // NumberFormat, DateTimeFormat, Collator, PluralRules, RelativeTimeFormat and ListFormat
    pub(super) intl_prototypes: Vec<ObjectHandle>,
    pub(super) dom: super::interfaces::DomInterfaces,
    pub(super) events: super::events::EventInterfaces,
    pub(super) ranges: super::ranges::RangeInterfaces,
    pub(super) observers: super::observers::ObserverInterfaces,
    pub(super) xhr: super::xhr::XhrInterfaces,
    pub(super) sockets: super::sockets::SocketInterfaces,
    pub(super) event_sources: super::eventsource::SourceInterfaces,
    pub(super) notices: super::notifications::NoticeInterfaces,
    pub(super) channels: super::broadcast::ChannelInterfaces,
    pub(super) readers: super::filereader::ReaderInterfaces,
    pub(super) ports: super::messaging::PortInterfaces,
    pub(super) traversal: super::traversal::TraversalInterfaces,
    pub(super) streams: super::streams::StreamInterfaces,
    pub(super) navigator_apis: super::navigator_apis::NavigatorInterfaces,
    // prototypes of interfaces whose objects only the engine makes, by name
    pub(super) hosts: HashMap<&'static str, ObjectHandle>,
    pub(super) perf_observers: super::perf_observers::PerfInterfaces,
    pub(super) paths: super::path2d::PathInterfaces,
    pub(super) matrices: super::matrices::MatrixInterfaces,
    pub(super) segmenters: super::segmenter::SegmenterInterfaces,
    pub(super) locales: super::locale::LocaleInterfaces,
    pub(super) display_names: super::displaynames::DisplayInterfaces,
    pub(super) duration_formats: super::durationformat::DurationInterfaces,
    pub(super) offscreen: super::offscreen::OffscreenInterfaces,
    pub(super) url_patterns: super::urlpattern::PatternInterfaces,
    pub(super) xpath: super::xpath::XPathInterfaces,
    pub(super) typed_om: super::typed_om::StyleOmInterfaces,
    pub(super) temporal: super::temporal::TemporalInterfaces,
    pub(super) disposables: super::disposables::DisposableInterfaces,
    pub(super) cssom: super::cssom::CssomInterfaces,
    pub(super) font_faces: super::fontfaces::FontInterfaces,
    pub(super) helpers: super::iterator_helpers::HelperInterfaces,
    pub(super) animations: super::animations::AnimInterfaces,
    pub(super) shapes: super::geometry_values::ShapeInterfaces,
    pub(super) urls: super::urls::UrlInterfaces,
    pub(super) audio: super::audio::AudioInterfaces,
}

fn plain(prototype: &ObjectHandle) -> ObjectHandle {
    new_object(JsObject::with_prototype(
        Some(prototype.clone()),
        ObjectClass::Ordinary,
    ))
}

fn fixed(value: Value) -> Property {
    Property {
        slot: Slot::Data(value),
        enumerable: false,
        writable: false,
        configurable: false,
    }
}

// the legacy constants of DOMException: the name with that code, its constant, and the code
pub(super) const DOM_EXCEPTION_CODES: &[(&str, &str, u16)] = &[
    ("IndexSizeError", "INDEX_SIZE_ERR", 1),
    ("", "DOMSTRING_SIZE_ERR", 2),
    ("HierarchyRequestError", "HIERARCHY_REQUEST_ERR", 3),
    ("WrongDocumentError", "WRONG_DOCUMENT_ERR", 4),
    ("InvalidCharacterError", "INVALID_CHARACTER_ERR", 5),
    ("", "NO_DATA_ALLOWED_ERR", 6),
    (
        "NoModificationAllowedError",
        "NO_MODIFICATION_ALLOWED_ERR",
        7,
    ),
    ("NotFoundError", "NOT_FOUND_ERR", 8),
    ("NotSupportedError", "NOT_SUPPORTED_ERR", 9),
    ("InUseAttributeError", "INUSE_ATTRIBUTE_ERR", 10),
    ("InvalidStateError", "INVALID_STATE_ERR", 11),
    ("SyntaxError", "SYNTAX_ERR", 12),
    ("InvalidModificationError", "INVALID_MODIFICATION_ERR", 13),
    ("NamespaceError", "NAMESPACE_ERR", 14),
    ("InvalidAccessError", "INVALID_ACCESS_ERR", 15),
    ("", "VALIDATION_ERR", 16),
    ("TypeMismatchError", "TYPE_MISMATCH_ERR", 17),
    ("SecurityError", "SECURITY_ERR", 18),
    ("NetworkError", "NETWORK_ERR", 19),
    ("AbortError", "ABORT_ERR", 20),
    ("URLMismatchError", "URL_MISMATCH_ERR", 21),
    ("QuotaExceededError", "QUOTA_EXCEEDED_ERR", 22),
    ("TimeoutError", "TIMEOUT_ERR", 23),
    ("InvalidNodeTypeError", "INVALID_NODE_TYPE_ERR", 24),
    ("DataCloneError", "DATA_CLONE_ERR", 25),
];

pub(super) struct Builder {
    function_prototype: ObjectHandle,
}

impl Builder {
    pub(super) fn function(&self, native: Native, name: &str, length: u32) -> ObjectHandle {
        let mut object = JsObject::with_prototype(
            Some(self.function_prototype.clone()),
            ObjectClass::Native(native),
        );
        object.define(
            "length".to_string(),
            Property {
                writable: false,
                ..Property::hidden(Value::Number(f64::from(length)))
            },
        );
        object.define(
            "name".to_string(),
            Property {
                writable: false,
                ..Property::hidden(Value::String(name.to_string().into()))
            },
        );
        new_object(object)
    }

    fn method(&self, target: &ObjectHandle, key: &str, native: Native, length: u32) {
        let name = match symbol_description(key) {
            Some(description) if is_symbol_key(key) => format!("[{description}]"),
            _ => key.to_string(),
        };
        let function = self.function(native, &name, length);
        target
            .borrow_mut()
            .define(key.to_string(), Property::hidden(Value::Object(function)));
    }

    pub(super) fn value(&self, target: &ObjectHandle, key: &str, value: Value) {
        // a toStringTag is configurable but not writable
        let writable = key != "@@toStringTag";
        target.borrow_mut().define(
            key.to_string(),
            Property {
                writable,
                ..Property::hidden(value)
            },
        );
    }

    pub(super) fn getter(
        &self,
        target: &ObjectHandle,
        key: &str,
        getter: Native,
        setter: Option<Native>,
    ) {
        let name = match symbol_description(key) {
            Some(description) if is_symbol_key(key) => format!("[{description}]"),
            _ => key.to_string(),
        };
        let get = Value::Object(self.function(getter, &format!("get {name}"), 0));
        let set =
            setter.map(|setter| Value::Object(self.function(setter, &format!("set {key}"), 1)));
        target.borrow_mut().define(
            key.to_string(),
            Property {
                slot: Slot::Accessor {
                    get: Some(get),
                    set,
                },
                enumerable: false,
                writable: false,
                configurable: true,
            },
        );
    }

    // a constructor function linked both ways with its prototype object
    pub(super) fn constructor(
        &self,
        native: Native,
        name: &str,
        length: u32,
        prototype: &ObjectHandle,
    ) -> ObjectHandle {
        let constructor = self.function(native, name, length);
        constructor.borrow_mut().define(
            "prototype".to_string(),
            fixed(Value::Object(prototype.clone())),
        );
        prototype.borrow_mut().define(
            "constructor".to_string(),
            Property::hidden(Value::Object(constructor.clone())),
        );
        constructor
    }
}

pub(super) fn create_intrinsics() -> (Intrinsics, Vec<(&'static str, Value)>) {
    let object_prototype = new_object(JsObject::with_prototype(None, ObjectClass::Ordinary));
    let function_prototype = new_object(JsObject::with_prototype(
        Some(object_prototype.clone()),
        ObjectClass::Native(Native::FunctionPrototype),
    ));
    let build = Builder {
        function_prototype: function_prototype.clone(),
    };
    let mut globals = Vec::new();

    for (key, native, length) in [
        ("hasOwnProperty", Native::ObjectHasOwnProperty, 1),
        ("isPrototypeOf", Native::ObjectIsPrototypeOf, 1),
        (
            "propertyIsEnumerable",
            Native::ObjectPropertyIsEnumerable,
            1,
        ),
        ("toString", Native::ObjectToString, 0),
        ("toLocaleString", Native::ObjectToLocaleString, 0),
        ("valueOf", Native::ObjectValueOf, 0),
        ("__defineGetter__", Native::ObjectDefineGetter, 2),
        ("__defineSetter__", Native::ObjectDefineSetter, 2),
        ("__lookupGetter__", Native::ObjectLookupGetter, 1),
        ("__lookupSetter__", Native::ObjectLookupSetter, 1),
    ] {
        build.method(&object_prototype, key, native, length);
    }
    build.getter(
        &object_prototype,
        "__proto__",
        Native::ObjectProtoGetter,
        Some(Native::ObjectProtoSetter),
    );
    let object = build.constructor(Native::ObjectConstructor, "Object", 1, &object_prototype);
    for (key, native, length) in [
        ("create", Native::ObjectCreate, 2),
        ("defineProperty", Native::ObjectDefineProperty, 3),
        ("defineProperties", Native::ObjectDefineProperties, 2),
        ("getPrototypeOf", Native::ObjectGetPrototypeOf, 1),
        ("setPrototypeOf", Native::ObjectSetPrototypeOf, 2),
        ("getOwnPropertyNames", Native::ObjectGetOwnPropertyNames, 1),
        (
            "getOwnPropertySymbols",
            Native::ObjectGetOwnPropertySymbols,
            1,
        ),
        (
            "getOwnPropertyDescriptor",
            Native::ObjectGetOwnPropertyDescriptor,
            2,
        ),
        (
            "getOwnPropertyDescriptors",
            Native::ObjectGetOwnPropertyDescriptors,
            1,
        ),
        ("keys", Native::ObjectKeys, 1),
        ("values", Native::ObjectValues, 1),
        ("entries", Native::ObjectEntries, 1),
        ("assign", Native::ObjectAssign, 2),
        ("fromEntries", Native::ObjectFromEntries, 1),
        ("is", Native::ObjectIs, 2),
        ("freeze", Native::ObjectFreeze, 1),
        ("isFrozen", Native::ObjectIsFrozen, 1),
        ("seal", Native::ObjectSeal, 1),
        ("isSealed", Native::ObjectIsSealed, 1),
        ("preventExtensions", Native::ObjectPreventExtensions, 1),
        ("isExtensible", Native::ObjectIsExtensible, 1),
        ("hasOwn", Native::ObjectHasOwn, 2),
        (
            "groupBy",
            Native::Collection(super::collections::CollectionMethod::GroupBy(false)),
            2,
        ),
    ] {
        build.method(&object, key, native, length);
    }
    globals.push(("Object", Value::Object(object)));

    for (key, native, length) in [
        ("call", Native::FunctionCall, 1),
        ("apply", Native::FunctionApply, 2),
        ("bind", Native::FunctionBind, 1),
        ("toString", Native::FunctionToString, 0),
    ] {
        build.method(&function_prototype, key, native, length);
    }
    function_prototype.borrow_mut().define(
        "@@hasInstance".to_string(),
        fixed(Value::Object(build.function(
            Native::FunctionHasInstance,
            "[Symbol.hasInstance]",
            1,
        ))),
    );
    build.value(
        &function_prototype,
        "name",
        Value::String(String::new().into()),
    );
    let function = build.constructor(
        Native::FunctionConstructor,
        "Function",
        1,
        &function_prototype,
    );
    globals.push(("Function", Value::Object(function)));

    let symbol_prototype = plain(&object_prototype);
    build.method(&symbol_prototype, "toString", Native::SymbolToString, 0);
    build.method(&symbol_prototype, "valueOf", Native::SymbolValueOf, 0);
    build.getter(
        &symbol_prototype,
        "description",
        Native::SymbolDescription,
        None,
    );
    build.value(
        &symbol_prototype,
        "@@toStringTag",
        Value::String("Symbol".to_string().into()),
    );
    let symbol = build.constructor(Native::SymbolConstructor, "Symbol", 0, &symbol_prototype);
    build.method(&symbol, "for", Native::SymbolFor, 1);
    build.method(&symbol, "keyFor", Native::SymbolKeyFor, 1);
    for name in [
        "iterator",
        "asyncIterator",
        "hasInstance",
        "toPrimitive",
        "toStringTag",
        "species",
        "isConcatSpreadable",
        "unscopables",
        "match",
        "matchAll",
        "replace",
        "search",
        "split",
        "dispose",
        "asyncDispose",
    ] {
        symbol
            .borrow_mut()
            .define(name.to_string(), fixed(Value::Symbol(format!("@@{name}"))));
    }
    globals.push(("Symbol", Value::Object(symbol)));

    let error_prototype = plain(&object_prototype);
    build.value(
        &error_prototype,
        "name",
        Value::String("Error".to_string().into()),
    );
    build.value(
        &error_prototype,
        "message",
        Value::String(String::new().into()),
    );
    build.method(&error_prototype, "toString", Native::ErrorToString, 0);
    let error = build.constructor(
        Native::ErrorConstructor("Error"),
        "Error",
        1,
        &error_prototype,
    );
    build.method(
        &error,
        "captureStackTrace",
        Native::ErrorCaptureStackTrace,
        1,
    );
    build.method(&error, "isError", Native::ErrorIsError, 1);
    globals.push(("Error", Value::Object(error.clone())));
    let mut error_prototypes = Vec::new();
    for kind in [
        "TypeError",
        "RangeError",
        "SyntaxError",
        "ReferenceError",
        "EvalError",
        "URIError",
        "AggregateError",
        "SuppressedError",
    ] {
        let prototype = plain(&error_prototype);
        build.value(&prototype, "name", Value::String(kind.to_string().into()));
        build.value(&prototype, "message", Value::String(String::new().into()));
        let length = match kind {
            "AggregateError" => 2,
            "SuppressedError" => 3,
            _ => 1,
        };
        let constructor =
            build.constructor(Native::ErrorConstructor(kind), kind, length, &prototype);
        constructor.borrow_mut().set_prototype(Some(error.clone()));
        globals.push((kind, Value::Object(constructor)));
        error_prototypes.push(prototype);
    }
    let [
        type_error_prototype,
        range_error_prototype,
        syntax_error_prototype,
        reference_error_prototype,
        eval_error_prototype,
        uri_error_prototype,
        aggregate_error_prototype,
        suppressed_error_prototype,
    ]: [ObjectHandle; 8] = error_prototypes
        .try_into()
        .unwrap_or_else(|_| unreachable!("eight error kinds"));

    let dom_exception_prototype = plain(&error_prototype);
    let dom_exception = build.constructor(
        Native::DomExceptionConstructor,
        "DOMException",
        0,
        &dom_exception_prototype,
    );
    for (_, constant, code) in DOM_EXCEPTION_CODES {
        for target in [&dom_exception, &dom_exception_prototype] {
            target
                .borrow_mut()
                .define(constant.to_string(), fixed(Value::Number(f64::from(*code))));
        }
    }
    build.value(
        &dom_exception_prototype,
        "@@toStringTag",
        Value::String("DOMException".into()),
    );
    globals.push(("DOMException", Value::Object(dom_exception)));

    let boolean_prototype = new_object(JsObject::with_prototype(
        Some(object_prototype.clone()),
        ObjectClass::Boolean(false),
    ));
    build.method(&boolean_prototype, "toString", Native::BooleanToString, 0);
    build.method(&boolean_prototype, "valueOf", Native::BooleanValueOf, 0);
    let boolean = build.constructor(Native::BooleanConstructor, "Boolean", 1, &boolean_prototype);
    globals.push(("Boolean", Value::Object(boolean)));

    let number_prototype = new_object(JsObject::with_prototype(
        Some(object_prototype.clone()),
        ObjectClass::Number(0.0),
    ));
    build.method(&number_prototype, "toString", Native::NumberToString, 1);
    build.method(&number_prototype, "valueOf", Native::NumberValueOf, 0);
    for (key, format, length) in [
        ("toFixed", super::numbers::NumberFormat::Fixed, 1),
        ("toPrecision", super::numbers::NumberFormat::Precision, 1),
        (
            "toExponential",
            super::numbers::NumberFormat::Exponential,
            1,
        ),
        ("toLocaleString", super::numbers::NumberFormat::Locale, 0),
    ] {
        build.method(&number_prototype, key, Native::NumberFormat(format), length);
    }
    let number = build.constructor(Native::NumberConstructor, "Number", 1, &number_prototype);
    let parse_int = Value::Object(build.function(Native::ParseInt, "parseInt", 2));
    let parse_float = Value::Object(build.function(Native::ParseFloat, "parseFloat", 1));
    build.value(&number, "parseInt", parse_int.clone());
    build.value(&number, "parseFloat", parse_float.clone());
    globals.push(("parseInt", parse_int));
    globals.push((
        "eval",
        Value::Object(build.function(Native::Eval, "eval", 1)),
    ));
    globals.push(("parseFloat", parse_float));
    for (name, native) in [
        ("encodeURI", Native::EncodeUri(false)),
        ("encodeURIComponent", Native::EncodeUri(true)),
        ("decodeURI", Native::DecodeUri(false)),
        ("decodeURIComponent", Native::DecodeUri(true)),
    ] {
        globals.push((name, Value::Object(build.function(native, name, 1))));
    }
    let css = plain(&object_prototype);
    build.method(&css, "escape", Native::CssEscape, 1);
    build.method(&css, "supports", Native::CssSupports, 1);
    let typed_om = super::typed_om::create(&build, &object_prototype, &css, &mut globals);
    globals.push(("CSS", Value::Object(css)));
    globals.push((
        "isNaN",
        Value::Object(build.function(Native::GlobalIsNaN, "isNaN", 1)),
    ));
    globals.push((
        "isFinite",
        Value::Object(build.function(Native::GlobalIsFinite, "isFinite", 1)),
    ));
    build.value(&number, "isFinite", Value::Builtin(Builtin::NumberIsFinite));
    build.method(&number, "isInteger", Native::NumberIsInteger, 1);
    build.method(&number, "isSafeInteger", Native::NumberIsSafeInteger, 1);
    build.method(&number, "isNaN", Native::NumberIsNaN, 1);
    for (name, value) in [
        ("MAX_SAFE_INTEGER", 9_007_199_254_740_991.0),
        ("MIN_SAFE_INTEGER", -9_007_199_254_740_991.0),
        ("EPSILON", f64::EPSILON),
        ("MAX_VALUE", f64::MAX),
        ("MIN_VALUE", 5e-324),
        ("NaN", f64::NAN),
        ("POSITIVE_INFINITY", f64::INFINITY),
        ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
    ] {
        number
            .borrow_mut()
            .define(name.to_string(), fixed(Value::Number(value)));
    }
    globals.push(("Number", Value::Object(number)));

    let string_prototype = new_object(JsObject::with_prototype(
        Some(object_prototype.clone()),
        ObjectClass::String(String::new()),
    ));
    build.method(&string_prototype, "toString", Native::StringToString, 0);
    build.method(&string_prototype, "valueOf", Native::StringToString, 0);
    for (key, method, length) in super::strings::STRING_METHODS {
        build.method(&string_prototype, key, Native::String(*method), *length);
    }
    let string = build.constructor(Native::StringConstructor, "String", 1, &string_prototype);
    build.method(&string, "fromCharCode", Native::StringFromCharCode, 1);
    build.method(&string, "fromCodePoint", Native::StringFromCodePoint, 1);
    build.method(&string, "raw", Native::StringRaw, 1);
    globals.push(("String", Value::Object(string)));

    let iterator_prototype = plain(&object_prototype);
    build.method(&iterator_prototype, "@@iterator", Native::IteratorSelf, 0);
    let async_iterator_prototype = plain(&object_prototype);
    build.method(
        &async_iterator_prototype,
        "@@asyncIterator",
        Native::IteratorSelf,
        0,
    );

    let array_prototype = plain(&object_prototype);
    let array_values = Value::Object(build.function(Native::ArrayValues, "values", 0));
    build.value(&array_prototype, "values", array_values.clone());
    build.value(&array_prototype, "@@iterator", array_values.clone());
    build.method(&array_prototype, "keys", Native::ArrayKeys, 0);
    build.method(&array_prototype, "entries", Native::ArrayEntries, 0);
    for (key, method, length) in super::arrays::ARRAY_METHODS {
        build.method(&array_prototype, key, Native::Array(*method), *length);
    }
    let array = build.constructor(Native::ArrayConstructor, "Array", 1, &array_prototype);
    build.value(&array, "isArray", Value::Builtin(Builtin::ArrayIsArray));
    build.method(&array, "from", Native::ArrayFrom, 1);
    build.method(&array, "of", Native::ArrayOf, 0);
    build.getter(&array, "@@species", Native::IteratorSelf, None);
    build.method(
        &array,
        "fromAsync",
        Native::Promise(Rc::new(super::promises::PromiseFunction::FromAsync)),
        1,
    );
    globals.push(("Array", Value::Object(array)));

    let array_iterator_prototype = plain(&iterator_prototype);
    build.method(
        &array_iterator_prototype,
        "next",
        Native::ArrayIteratorNext,
        0,
    );
    build.value(
        &array_iterator_prototype,
        "@@toStringTag",
        Value::String("Array Iterator".into()),
    );

    let generator_prototype = plain(&iterator_prototype);
    build.method(&generator_prototype, "next", Native::GeneratorNext, 1);
    build.method(&generator_prototype, "return", Native::GeneratorReturn, 1);
    build.method(&generator_prototype, "throw", Native::GeneratorThrow, 1);
    build.value(
        &generator_prototype,
        "@@toStringTag",
        Value::String("Generator".to_string().into()),
    );
    let generator_function_prototype = plain(&function_prototype);
    generator_function_prototype.borrow_mut().define(
        "prototype".to_string(),
        Property {
            writable: false,
            ..Property::hidden(Value::Object(generator_prototype.clone()))
        },
    );

    let async_generator_prototype = plain(&async_iterator_prototype);
    build.method(
        &async_generator_prototype,
        "next",
        Native::AsyncGeneratorNext,
        1,
    );
    build.method(
        &async_generator_prototype,
        "return",
        Native::AsyncGeneratorReturn,
        1,
    );
    build.method(
        &async_generator_prototype,
        "throw",
        Native::AsyncGeneratorThrow,
        1,
    );
    build.value(
        &async_generator_prototype,
        "@@toStringTag",
        Value::String("AsyncGenerator".to_string().into()),
    );
    let async_generator_function_prototype = plain(&function_prototype);
    async_generator_function_prototype.borrow_mut().define(
        "prototype".to_string(),
        Property {
            writable: false,
            ..Property::hidden(Value::Object(async_generator_prototype.clone()))
        },
    );
    let async_function_prototype = plain(&function_prototype);
    build.value(
        &async_function_prototype,
        "@@toStringTag",
        Value::String("AsyncFunction".to_string().into()),
    );

    let promise_prototype = plain(&object_prototype);
    build.value(
        &promise_prototype,
        "@@toStringTag",
        Value::String("Promise".to_string().into()),
    );
    use super::promises::PromiseFunction as P;
    let promise_native = |function| Native::Promise(Rc::new(function));
    for (key, function, length) in [
        ("then", P::Then, 2),
        ("catch", P::Catch, 1),
        ("finally", P::Finally, 1),
    ] {
        build.method(&promise_prototype, key, promise_native(function), length);
    }
    let promise_constructor = build.constructor(
        promise_native(P::Constructor),
        "Promise",
        1,
        &promise_prototype,
    );
    for (key, function, length) in [
        ("resolve", P::Resolve, 1),
        ("reject", P::Reject, 1),
        ("all", P::Combinator(super::promises::Combinator::All), 1),
        (
            "allSettled",
            P::Combinator(super::promises::Combinator::AllSettled),
            1,
        ),
        ("any", P::Combinator(super::promises::Combinator::Any), 1),
        ("race", P::Combinator(super::promises::Combinator::Race), 1),
        ("withResolvers", P::WithResolvers, 0),
        ("try", P::Try, 1),
    ] {
        build.method(&promise_constructor, key, promise_native(function), length);
    }
    build.getter(
        &promise_constructor,
        "@@species",
        promise_native(P::Species),
        None,
    );
    globals.push(("Promise", Value::Object(promise_constructor.clone())));
    let regexp_prototype = plain(&object_prototype);
    for (key, method, length) in super::regexp::REGEXP_METHODS {
        build.method(&regexp_prototype, key, Native::RegExp(*method), *length);
    }
    for (key, method) in super::regexp::REGEXP_GETTERS {
        build.getter(&regexp_prototype, key, Native::RegExp(*method), None);
    }
    let regexp = build.constructor(
        Native::RegExp(super::regexp::RegExpMethod::Constructor),
        "RegExp",
        2,
        &regexp_prototype,
    );
    build.method(&regexp, "escape", Native::RegExpEscape, 1);
    globals.push(("RegExp", Value::Object(regexp)));

    let math = plain(&object_prototype);
    for (key, function, length) in super::numbers::MATH_FUNCTIONS {
        build.method(&math, key, Native::Math(*function), *length);
    }
    for (key, value) in super::numbers::MATH_CONSTANTS {
        math.borrow_mut()
            .define(key.to_string(), fixed(Value::Number(*value)));
    }
    build.value(&math, "@@toStringTag", Value::String("Math".into()));
    globals.push(("Math", Value::Object(math)));

    let json = plain(&object_prototype);
    build.method(&json, "parse", Native::JsonParse, 2);
    build.method(&json, "stringify", Native::JsonStringify, 3);
    build.method(&json, "rawJSON", Native::JsonRawJson, 1);
    build.method(&json, "isRawJSON", Native::JsonIsRawJson, 1);
    build.value(&json, "@@toStringTag", Value::String("JSON".into()));
    globals.push(("JSON", Value::Object(json)));

    let atomics = plain(&object_prototype);
    for (key, op, length) in super::atomics::ATOMICS_METHODS {
        build.method(&atomics, key, Native::Atomics(*op), *length);
    }
    build.value(&atomics, "@@toStringTag", Value::String("Atomics".into()));
    globals.push(("Atomics", Value::Object(atomics)));

    let console = plain(&object_prototype);
    for (key, method) in super::console::CONSOLE_METHODS {
        build.method(&console, key, Native::Console(*method), 0);
    }
    globals.push(("console", Value::Object(console)));

    use super::collections::{CollectionKind, CollectionMethod as C, IterationKind};
    let collection_iterator_prototype = plain(&iterator_prototype);
    build.method(
        &collection_iterator_prototype,
        "next",
        Native::Collection(C::CursorNext),
        0,
    );
    build.value(
        &collection_iterator_prototype,
        "@@toStringTag",
        Value::String("Map Iterator".into()),
    );

    let map_prototype = plain(&object_prototype);
    for (key, method, length) in [
        ("get", C::Get, 1),
        ("set", C::Set, 2),
        ("has", C::Has, 1),
        ("delete", C::Delete, 1),
        ("clear", C::Clear, 0),
        ("forEach", C::ForEach, 1),
        ("keys", C::Iterate(IterationKind::Keys), 0),
        ("values", C::Iterate(IterationKind::Values), 0),
    ] {
        build.method(&map_prototype, key, Native::Collection(method), length);
    }
    let map_entries = Value::Object(build.function(
        Native::Collection(C::Iterate(IterationKind::Entries)),
        "entries",
        0,
    ));
    build.value(&map_prototype, "entries", map_entries.clone());
    build.value(&map_prototype, "@@iterator", map_entries);
    build.getter(&map_prototype, "size", Native::Collection(C::Size), None);
    build.value(&map_prototype, "@@toStringTag", Value::String("Map".into()));
    let map = build.constructor(
        Native::Collection(C::Constructor(CollectionKind::Map)),
        "Map",
        0,
        &map_prototype,
    );
    build.method(&map, "groupBy", Native::Collection(C::GroupBy(true)), 2);
    globals.push(("Map", Value::Object(map)));

    let set_prototype = plain(&object_prototype);
    for (key, method, length) in [
        ("add", C::Add, 1),
        ("has", C::Has, 1),
        ("delete", C::Delete, 1),
        ("clear", C::Clear, 0),
        ("forEach", C::ForEach, 1),
        ("entries", C::Iterate(IterationKind::Entries), 0),
        ("union", C::Union, 1),
        ("intersection", C::Intersection, 1),
        ("difference", C::Difference, 1),
        ("symmetricDifference", C::SymmetricDifference, 1),
        ("isSubsetOf", C::IsSubsetOf, 1),
        ("isSupersetOf", C::IsSupersetOf, 1),
        ("isDisjointFrom", C::IsDisjointFrom, 1),
    ] {
        build.method(&set_prototype, key, Native::Collection(method), length);
    }
    let set_values = Value::Object(build.function(
        Native::Collection(C::Iterate(IterationKind::Values)),
        "values",
        0,
    ));
    build.value(&set_prototype, "values", set_values.clone());
    build.value(&set_prototype, "keys", set_values.clone());
    build.value(&set_prototype, "@@iterator", set_values);
    build.getter(&set_prototype, "size", Native::Collection(C::Size), None);
    build.value(&set_prototype, "@@toStringTag", Value::String("Set".into()));
    let set = build.constructor(
        Native::Collection(C::Constructor(CollectionKind::Set)),
        "Set",
        0,
        &set_prototype,
    );
    globals.push(("Set", Value::Object(set)));

    let weak_map_prototype = plain(&object_prototype);
    for (key, method, length) in [
        ("get", C::Get, 1),
        ("set", C::Set, 2),
        ("has", C::Has, 1),
        ("delete", C::Delete, 1),
    ] {
        build.method(&weak_map_prototype, key, Native::Collection(method), length);
    }
    build.value(
        &weak_map_prototype,
        "@@toStringTag",
        Value::String("WeakMap".into()),
    );
    let weak_map = build.constructor(
        Native::Collection(C::Constructor(CollectionKind::WeakMap)),
        "WeakMap",
        0,
        &weak_map_prototype,
    );
    globals.push(("WeakMap", Value::Object(weak_map)));

    let weak_set_prototype = plain(&object_prototype);
    for (key, method, length) in [
        ("add", C::Add, 1),
        ("has", C::Has, 1),
        ("delete", C::Delete, 1),
    ] {
        build.method(&weak_set_prototype, key, Native::Collection(method), length);
    }
    build.value(
        &weak_set_prototype,
        "@@toStringTag",
        Value::String("WeakSet".into()),
    );
    let weak_set = build.constructor(
        Native::Collection(C::Constructor(CollectionKind::WeakSet)),
        "WeakSet",
        0,
        &weak_set_prototype,
    );
    globals.push(("WeakSet", Value::Object(weak_set)));

    let weak_ref_prototype = plain(&object_prototype);
    build.method(
        &weak_ref_prototype,
        "deref",
        Native::Collection(C::WeakRefDeref),
        0,
    );
    build.value(
        &weak_ref_prototype,
        "@@toStringTag",
        Value::String("WeakRef".into()),
    );
    let weak_ref = build.constructor(
        Native::Collection(C::WeakRefConstructor),
        "WeakRef",
        1,
        &weak_ref_prototype,
    );
    globals.push(("WeakRef", Value::Object(weak_ref)));
    let registry_prototype = plain(&object_prototype);
    build.method(
        &registry_prototype,
        "register",
        Native::Collection(C::Register),
        2,
    );
    build.method(
        &registry_prototype,
        "unregister",
        Native::Collection(C::Unregister),
        1,
    );
    build.value(
        &registry_prototype,
        "@@toStringTag",
        Value::String("FinalizationRegistry".into()),
    );
    let registry = build.constructor(
        Native::Collection(C::RegistryConstructor),
        "FinalizationRegistry",
        1,
        &registry_prototype,
    );
    globals.push(("FinalizationRegistry", Value::Object(registry)));
    globals.push((
        "reportError",
        Value::Object(build.function(Native::ReportError, "reportError", 1)),
    ));

    let date_prototype = plain(&object_prototype);
    for (key, method, length) in super::dates::DATE_METHODS {
        build.method(&date_prototype, key, Native::Date(*method), *length);
    }
    date_prototype.borrow_mut().define(
        "@@toPrimitive".to_string(),
        Property {
            writable: false,
            ..Property::hidden(Value::Object(build.function(
                Native::Date(super::dates::DateMethod::ToPrimitive),
                "[Symbol.toPrimitive]",
                1,
            )))
        },
    );
    let date = build.constructor(
        Native::Date(super::dates::DateMethod::Constructor),
        "Date",
        7,
        &date_prototype,
    );
    build.method(&date, "now", Native::Date(super::dates::DateMethod::Now), 0);
    build.method(
        &date,
        "parse",
        Native::Date(super::dates::DateMethod::Parse),
        1,
    );
    build.method(&date, "UTC", Native::Date(super::dates::DateMethod::Utc), 7);
    globals.push(("Date", Value::Object(date)));
    let temporal =
        super::temporal::create(&build, &object_prototype, &date_prototype, &mut globals);

    use super::typed::{ELEMENT_KINDS, TYPED_METHODS, TypedMethod as T, VIEW_ACCESSORS};
    let typed_prototype = plain(&object_prototype);
    for (key, method, length) in TYPED_METHODS {
        build.method(&typed_prototype, key, Native::Typed(*method), *length);
    }
    let typed_values = Value::Object(build.function(Native::Typed(T::Values), "values", 0));
    build.value(&typed_prototype, "values", typed_values.clone());
    build.value(&typed_prototype, "@@iterator", typed_values);
    for (key, method) in [
        ("buffer", T::Buffer),
        ("byteLength", T::ByteLength),
        ("byteOffset", T::ByteOffset),
        ("length", T::Length),
        ("@@toStringTag", T::ToStringTag),
    ] {
        build.getter(&typed_prototype, key, Native::Typed(method), None);
    }
    let typed_constructor = build.constructor(
        Native::Typed(T::AbstractConstructor),
        "TypedArray",
        0,
        &typed_prototype,
    );
    build.method(&typed_constructor, "from", Native::Typed(T::From), 1);
    build.method(&typed_constructor, "of", Native::Typed(T::Of), 0);
    let mut typed_prototypes = Vec::new();
    for (kind, name) in ELEMENT_KINDS {
        let prototype = plain(&typed_prototype);
        prototype.borrow_mut().define(
            "BYTES_PER_ELEMENT".to_string(),
            fixed(Value::Number(kind.size() as f64)),
        );
        let constructor =
            build.constructor(Native::Typed(T::Constructor(*kind)), name, 3, &prototype);
        constructor
            .borrow_mut()
            .set_prototype(Some(typed_constructor.clone()));
        constructor.borrow_mut().define(
            "BYTES_PER_ELEMENT".to_string(),
            fixed(Value::Number(kind.size() as f64)),
        );
        if *kind == super::typed::ElementKind::Uint8 {
            use super::base64::TextMethod as X;
            for (target, key, method, length) in [
                (&prototype, "toBase64", X::ToBase64, 0),
                (&prototype, "toHex", X::ToHex, 0),
                (&prototype, "setFromBase64", X::SetFromBase64, 1),
                (&prototype, "setFromHex", X::SetFromHex, 1),
                (&constructor, "fromBase64", X::FromBase64, 1),
                (&constructor, "fromHex", X::FromHex, 1),
            ] {
                build.method(target, key, Native::Typed(T::Text(method)), length);
            }
        }
        globals.push((name, Value::Object(constructor)));
        typed_prototypes.push(prototype);
    }
    let array_buffer_prototype = plain(&object_prototype);
    build.getter(
        &array_buffer_prototype,
        "byteLength",
        Native::Typed(T::BufferByteLength),
        None,
    );
    build.method(
        &array_buffer_prototype,
        "slice",
        Native::Typed(T::BufferSlice),
        2,
    );
    for (key, member) in [
        ("maxByteLength", T::BufferMaxByteLength),
        ("resizable", T::BufferResizable),
        ("detached", T::BufferDetached),
    ] {
        build.getter(&array_buffer_prototype, key, Native::Typed(member), None);
    }
    for (key, member) in [
        ("resize", T::BufferResize),
        ("transfer", T::BufferTransfer(true)),
        ("transferToFixedLength", T::BufferTransfer(false)),
    ] {
        build.method(&array_buffer_prototype, key, Native::Typed(member), 1);
    }
    build.value(
        &array_buffer_prototype,
        "@@toStringTag",
        Value::String("ArrayBuffer".into()),
    );
    let array_buffer = build.constructor(
        Native::Typed(T::BufferConstructor),
        "ArrayBuffer",
        1,
        &array_buffer_prototype,
    );
    build.method(&array_buffer, "isView", Native::Typed(T::BufferIsView), 1);
    globals.push(("ArrayBuffer", Value::Object(array_buffer)));
    let data_view_prototype = plain(&object_prototype);
    for (key, method) in [
        ("buffer", T::ViewBuffer),
        ("byteLength", T::ViewByteLength),
        ("byteOffset", T::ViewByteOffset),
    ] {
        build.getter(&data_view_prototype, key, Native::Typed(method), None);
    }
    for (name, kind) in VIEW_ACCESSORS {
        build.method(
            &data_view_prototype,
            &format!("get{name}"),
            Native::Typed(T::ViewGet(*kind)),
            1,
        );
        build.method(
            &data_view_prototype,
            &format!("set{name}"),
            Native::Typed(T::ViewSet(*kind)),
            2,
        );
    }
    build.value(
        &data_view_prototype,
        "@@toStringTag",
        Value::String("DataView".into()),
    );
    let data_view = build.constructor(
        Native::Typed(T::ViewConstructor),
        "DataView",
        1,
        &data_view_prototype,
    );
    globals.push(("DataView", Value::Object(data_view)));

    let bigint_prototype = plain(&object_prototype);
    build.method(&bigint_prototype, "toString", Native::BigIntToString, 0);
    build.method(
        &bigint_prototype,
        "toLocaleString",
        Native::BigIntToString,
        0,
    );
    build.method(&bigint_prototype, "valueOf", Native::BigIntValueOf, 0);
    build.value(
        &bigint_prototype,
        "@@toStringTag",
        Value::String("BigInt".into()),
    );
    let bigint = build.constructor(Native::BigIntCall, "BigInt", 1, &bigint_prototype);
    build.method(&bigint, "asIntN", Native::BigIntAsN(true), 2);
    build.method(&bigint, "asUintN", Native::BigIntAsN(false), 2);
    globals.push(("BigInt", Value::Object(bigint)));

    use super::encoding::CodecMethod as E;
    let text_encoder_prototype = plain(&object_prototype);
    build.method(
        &text_encoder_prototype,
        "encode",
        Native::Codec(E::Encode),
        0,
    );
    build.method(
        &text_encoder_prototype,
        "encodeInto",
        Native::Codec(E::EncodeInto),
        2,
    );
    build.getter(
        &text_encoder_prototype,
        "encoding",
        Native::Codec(E::EncoderEncoding),
        None,
    );
    let text_encoder = build.constructor(
        Native::Codec(E::EncoderConstructor),
        "TextEncoder",
        0,
        &text_encoder_prototype,
    );
    globals.push(("TextEncoder", Value::Object(text_encoder)));
    let text_decoder_prototype = plain(&object_prototype);
    build.method(
        &text_decoder_prototype,
        "decode",
        Native::Codec(E::Decode),
        0,
    );
    for (key, method) in [
        ("encoding", E::DecoderEncoding),
        ("fatal", E::DecoderFatal),
        ("ignoreBOM", E::DecoderIgnoreBom),
    ] {
        build.getter(&text_decoder_prototype, key, Native::Codec(method), None);
    }
    let text_decoder = build.constructor(
        Native::Codec(E::DecoderConstructor),
        "TextDecoder",
        0,
        &text_decoder_prototype,
    );
    globals.push(("TextDecoder", Value::Object(text_decoder)));

    use super::intl::IntlMethod as I;
    let intl = plain(&object_prototype);
    build.value(&intl, "@@toStringTag", Value::String("Intl".into()));
    build.method(
        &intl,
        "getCanonicalLocales",
        Native::Intl(Rc::new(I::GetCanonicalLocales)),
        1,
    );
    let mut intl_prototypes = Vec::new();
    for (name, constructor, extra) in [
        ("NumberFormat", I::NumberFormatConstructor, "format"),
        ("DateTimeFormat", I::DateTimeFormatConstructor, "format"),
        ("Collator", I::CollatorConstructor, "compare"),
        ("PluralRules", I::PluralRulesConstructor, "select"),
        (
            "RelativeTimeFormat",
            I::RelativeTimeFormatConstructor,
            "format",
        ),
        ("ListFormat", I::ListFormatConstructor, "format"),
    ] {
        let prototype = plain(&object_prototype);
        match (name, extra) {
            ("NumberFormat" | "DateTimeFormat", _) => build.getter(
                &prototype,
                "format",
                Native::Intl(Rc::new(I::FormatGetter)),
                None,
            ),
            ("Collator", _) => build.getter(
                &prototype,
                "compare",
                Native::Intl(Rc::new(I::CompareGetter)),
                None,
            ),
            ("PluralRules", _) => {
                build.method(&prototype, "select", Native::Intl(Rc::new(I::Select)), 1)
            }
            ("ListFormat", _) => {
                build.method(
                    &prototype,
                    "format",
                    Native::Intl(Rc::new(I::ListFormat)),
                    1,
                );
                build.method(
                    &prototype,
                    "formatToParts",
                    Native::Intl(Rc::new(I::ListFormatToParts)),
                    1,
                );
            }
            _ => build.method(
                &prototype,
                "format",
                Native::Intl(Rc::new(I::RelativeFormat)),
                2,
            ),
        }
        build.method(
            &prototype,
            "resolvedOptions",
            Native::Intl(Rc::new(I::ResolvedOptions)),
            0,
        );
        build.value(
            &prototype,
            "@@toStringTag",
            Value::String(format!("Intl.{name}").into()),
        );
        let function = build.constructor(Native::Intl(Rc::new(constructor)), name, 0, &prototype);
        build.method(
            &function,
            "supportedLocalesOf",
            Native::Intl(Rc::new(I::SupportedLocalesOf)),
            1,
        );
        intl.borrow_mut()
            .define(name.to_string(), Property::hidden(Value::Object(function)));
        intl_prototypes.push(prototype);
    }
    let mut hosts = HashMap::new();
    for name in HOST_INTERFACES {
        let prototype = plain(&object_prototype);
        build.value(&prototype, "@@toStringTag", Value::String((*name).into()));
        let constructor = build.constructor(Native::IllegalConstructor, name, 0, &prototype);
        globals.push((name, Value::Object(constructor)));
        hosts.insert(*name, prototype);
    }
    let transfer = plain(&object_prototype);
    build.value(
        &transfer,
        "@@toStringTag",
        Value::String("DataTransfer".into()),
    );
    let constructor = build.constructor(
        Native::DataTransferConstructor,
        "DataTransfer",
        0,
        &transfer,
    );
    globals.push(("DataTransfer", Value::Object(constructor)));
    hosts.insert("DataTransfer", transfer);
    for name in [
        "offsetLeft",
        "offsetTop",
        "pageLeft",
        "pageTop",
        "width",
        "height",
        "scale",
    ] {
        build.getter(
            &hosts["VisualViewport"],
            name,
            Native::Window(super::geometry::WindowMethod::VisualViewport(name)),
            None,
        );
    }
    let perf_observers = super::perf_observers::create(&build, &object_prototype, &mut globals);
    let segmenters =
        super::segmenter::create(&build, &object_prototype, &iterator_prototype, &intl);
    let locales = super::locale::create(&build, &object_prototype, &intl);
    let display_names = super::displaynames::create(&build, &object_prototype, &intl);
    let duration_formats = super::durationformat::create(&build, &object_prototype, &intl);
    let offscreen = super::offscreen::create(&build, &object_prototype, &mut globals);
    let url_patterns = super::urlpattern::create(&build, &object_prototype, &mut globals);
    let xpath = super::xpath::create(&build, &object_prototype, &mut globals);
    let (disposables, disposable_globals) = super::disposables::create(&build, &object_prototype);
    globals.extend(disposable_globals);
    globals.push(("Intl", Value::Object(intl)));

    let reflect = plain(&object_prototype);
    for (key, method, length) in super::reflect::REFLECT_METHODS {
        build.method(&reflect, key, Native::Reflect(*method), *length);
    }
    build.value(&reflect, "@@toStringTag", Value::String("Reflect".into()));
    globals.push(("Reflect", Value::Object(reflect)));
    let proxy = build.function(
        Native::Reflect(super::reflect::ReflectMethod::ProxyConstructor),
        "Proxy",
        2,
    );
    build.method(
        &proxy,
        "revocable",
        Native::Reflect(super::reflect::ReflectMethod::ProxyRevocable),
        2,
    );
    globals.push(("Proxy", Value::Object(proxy)));

    globals.push(("globalThis", Value::Window));

    let dom = super::interfaces::create(&build, &object_prototype, &mut globals);
    let events = super::events::create(&build, &object_prototype, &mut globals);
    let ranges = super::ranges::create(&build, &object_prototype, &mut globals);
    let observers = super::observers::create(&build, &object_prototype, &mut globals);
    let xhr = super::xhr::create(&build, &dom.prototype("EventTarget"), &mut globals);
    let sockets = super::sockets::create(&build, &dom.prototype("EventTarget"), &mut globals);
    let event_sources =
        super::eventsource::create(&build, &dom.prototype("EventTarget"), &mut globals);
    let notices = super::notifications::create(&build, &dom.prototype("EventTarget"), &mut globals);
    let channels = super::broadcast::create(&build, &dom.prototype("EventTarget"), &mut globals);
    let readers = super::filereader::create(&build, &dom.prototype("EventTarget"), &mut globals);
    let traversal = super::traversal::create(&build, &object_prototype, &mut globals);
    let helpers = super::iterator_helpers::create(&build, &iterator_prototype, &mut globals);
    super::markup::create(&build, &object_prototype, &mut globals);
    let paths = super::path2d::create(&build, &object_prototype, &mut globals);
    let cssom = super::cssom::create(&build, &object_prototype, &mut globals);
    let font_faces = super::fontfaces::create(
        &build,
        &object_prototype,
        &dom.prototype("EventTarget"),
        &mut globals,
    );
    let navigator_apis = super::navigator_apis::create(
        &build,
        &object_prototype,
        &dom.prototype("EventTarget"),
        &mut globals,
    );
    navigator_apis
        .locks
        .borrow_mut()
        .set_prototype(Some(hosts["LockManager"].clone()));
    let streams = super::streams::create(
        &build,
        &object_prototype,
        &async_iterator_prototype,
        &mut globals,
    );
    let animations = super::animations::create(&build, &dom.prototype("EventTarget"), &mut globals);
    let clipboard_item = build.constructor(
        Native::ClipboardItemConstructor,
        "ClipboardItem",
        1,
        &plain(&object_prototype),
    );
    globals.push(("ClipboardItem", Value::Object(clipboard_item)));
    let shapes = super::geometry_values::create(&build, &object_prototype, &mut globals);
    let matrices = super::matrices::create(
        &build,
        &object_prototype,
        &shapes.point_read_only,
        &mut globals,
    );
    let ports = super::messaging::create(
        &build,
        &dom.prototype("EventTarget"),
        &object_prototype,
        &mut globals,
    );
    let urls = super::urls::create(&build, &object_prototype, &mut globals);
    let audio = super::audio::create(
        &build,
        &dom.prototype("EventTarget"),
        &object_prototype,
        &mut globals,
    );
    let intrinsics = Intrinsics {
        object_prototype,
        function_prototype,
        async_function_prototype,
        generator_function_prototype,
        async_generator_function_prototype,
        generator_prototype,
        async_generator_prototype,
        array_prototype,
        string_prototype,
        number_prototype,
        boolean_prototype,
        symbol_prototype,
        error_prototype,
        type_error_prototype,
        range_error_prototype,
        syntax_error_prototype,
        reference_error_prototype,
        eval_error_prototype,
        uri_error_prototype,
        aggregate_error_prototype,
        suppressed_error_prototype,
        dom_exception_prototype,
        promise_prototype,
        promise_constructor,
        regexp_prototype,
        array_values,
        iterator_prototype,
        array_iterator_prototype,
        map_prototype,
        set_prototype,
        weak_map_prototype,
        weak_set_prototype,
        weak_ref_prototype,
        registry_prototype,
        collection_iterator_prototype,
        date_prototype,
        typed_prototypes,
        array_buffer_prototype,
        data_view_prototype,
        text_encoder_prototype,
        text_decoder_prototype,
        bigint_prototype,
        intl_prototypes,
        dom,
        events,
        ranges,
        observers,
        xhr,
        sockets,
        event_sources,
        notices,
        channels,
        readers,
        ports,
        traversal,
        streams,
        navigator_apis,
        paths,
        matrices,
        segmenters,
        locales,
        display_names,
        duration_formats,
        offscreen,
        url_patterns,
        xpath,
        typed_om,
        temporal,
        hosts,
        perf_observers,
        disposables,
        cssom,
        font_faces,
        helpers,
        animations,
        shapes,
        urls,
        audio,
    };
    (intrinsics, globals)
}

fn argument(arguments: &[Value], index: usize) -> Value {
    arguments.get(index).cloned().unwrap_or(Value::Undefined)
}

impl ScriptRuntime {
    // a function from source text, as the Function constructor and on* attributes build one
    pub(super) fn compile_function_text(
        &mut self,
        parameters: &str,
        body: &str,
    ) -> Result<Value, ScriptError> {
        let (function, source) =
            super::parser::parse_function_source(parameters, body, false, false)
                .map_err(|error| self.throw_error("SyntaxError", error.message))?;
        let code = Compiler::new(&source)
            .compile_function(&function)
            .map_err(|error| self.throw_error("SyntaxError", error.message))?;
        Ok(self.new_closure(code, None, None))
    }

    // an indirect eval: its source runs as sloppy global code
    fn evaluate_indirect(&mut self, source: &str) -> Result<Value, ScriptError> {
        let (code, scope) = self.instantiate_eval(source, false, EvalContext::default(), None)?;
        self.run_eval_code(code, scope)
    }

    // eval code compiled over the scope chain it runs on, with its declarations
    // instantiated: a sloppy one's vars and functions may not take a name a lexical
    // binding holds between it and its variable environment, a global one must be
    // declarable on the global, and a function's variable environment grows by the
    // names it lacks
    pub(super) fn instantiate_eval(
        &mut self,
        source: &str,
        strict: bool,
        context: EvalContext,
        scope: Option<ScopeRef>,
    ) -> Result<(Rc<Code>, ScopeRef), ScriptError> {
        let script = super::parser::parse_eval(source, strict, context)
            .map_err(|error| self.throw_error("SyntaxError", error.message))?;
        let mut chain = Vec::new();
        let mut walk = scope.clone();
        while let Some(current) = walk {
            walk = current.borrow().parent.clone();
            chain.push(current);
        }
        let caller: Vec<CallerScope> = chain
            .iter()
            .map(|scope| {
                let scope = scope.borrow();
                CallerScope {
                    layout: scope.layout.clone(),
                    extra: scope.extra.clone(),
                }
            })
            .collect();
        let eval = Compiler::new(source)
            .compile_eval(&script, &caller, context)
            .map_err(|error| self.throw_error("SyntaxError", error.message))?;
        if !script.strict {
            self.check_eval_declarations(&chain, &eval)?;
        }
        if let Some(target) = chain
            .iter()
            .find(|scope| scope.borrow().layout.role == ScopeRole::Var)
        {
            let mut target = target.borrow_mut();
            for name in eval.grown {
                target.slots.push(Value::Undefined);
                target.extra.push(name);
            }
        }
        let scope = Scope::new(eval.code.layout.clone(), scope);
        Ok((eval.code, scope))
    }

    fn check_eval_declarations(
        &mut self,
        chain: &[ScopeRef],
        eval: &EvalCode,
    ) -> Result<(), ScriptError> {
        let declared: HashSet<&str> = eval.declared.iter().map(|name| &**name).collect();
        for scope in chain {
            let layout = scope.borrow().layout.clone();
            // annex b lets a var share a catch parameter's name
            if matches!(layout.role, ScopeRole::With | ScopeRole::Catch) {
                continue;
            }
            let var_scope = layout.role == ScopeRole::Var;
            for (name, kind) in layout.names.iter().zip(layout.kinds.iter()) {
                let lexical = match kind {
                    BindingKind::Let | BindingKind::Const | BindingKind::Class => true,
                    BindingKind::Hidden => false,
                    _ => !var_scope,
                };
                if lexical && declared.contains(&**name) {
                    return Err(self.redeclaration(name));
                }
            }
            if var_scope {
                return Ok(());
            }
        }
        let lexical = {
            let global = self.global.borrow();
            eval.declared
                .iter()
                .find(|name| global.lexicals.contains(&***name))
                .cloned()
        };
        if let Some(name) = lexical {
            return Err(self.redeclaration(&name));
        }
        // the global object here is always extensible, so CanDeclareGlobalVar holds for
        // every name and CanDeclareGlobalFunction fails only on a property a function
        // may not redefine
        for name in &eval.functions {
            let declarable = match self.own_property_of(&Value::Window, name)? {
                None => true,
                Some(property) => {
                    property.configurable
                        || matches!(property.slot, Slot::Data(_))
                            && property.writable
                            && property.enumerable
                }
            };
            if !declarable {
                return Err(self.type_error(format!("cannot declare the global function {name}")));
            }
        }
        Ok(())
    }

    fn redeclaration(&mut self, name: &str) -> ScriptError {
        self.throw_error(
            "SyntaxError",
            format!("identifier '{name}' has already been declared"),
        )
    }

    // a native function object created at run time, such as a proxy's revoke function
    pub(super) fn native_function(&self, native: Native, name: &str, length: u32) -> Value {
        Value::Object(
            Builder {
                function_prototype: self.intrinsics.function_prototype.clone(),
            }
            .function(native, name, length),
        )
    }

    pub(super) fn is_constructor(value: &Value) -> bool {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Function(closure) => {
                    matches!(
                        closure.code.kind,
                        FunctionKind::Normal
                            | FunctionKind::ClassConstructor
                            | FunctionKind::DerivedConstructor
                    ) && !closure.code.is_async
                        && !closure.code.is_generator
                }
                ObjectClass::Bound { target, .. } => Self::is_constructor(target),
                ObjectClass::Native(native) => native.is_constructor(),
                _ => false,
            },
            Value::Builtin(_) => false,
            other => is_callable(other),
        }
    }

    pub(super) fn descriptor_value(&self, property: &Property) -> Value {
        let object = self.new_plain_object();
        {
            let mut object = object.borrow_mut();
            match &property.slot {
                Slot::Data(value) => {
                    object.insert("value".to_string(), value.resolve());
                    object.insert("writable".to_string(), Value::Bool(property.writable));
                }
                Slot::Accessor { get, set } => {
                    object.insert("get".to_string(), get.clone().unwrap_or(Value::Undefined));
                    object.insert("set".to_string(), set.clone().unwrap_or(Value::Undefined));
                }
            }
            object.insert("enumerable".to_string(), Value::Bool(property.enumerable));
            object.insert(
                "configurable".to_string(),
                Value::Bool(property.configurable),
            );
        }
        Value::Object(object)
    }

    // the own property of any value, as getOwnPropertyDescriptor and hasOwnProperty see it
    pub(super) fn own_property_of(
        &mut self,
        value: &Value,
        key: &str,
    ) -> Result<Option<Property>, ScriptError> {
        if let Some(object) = self.wrapper_object(value) {
            return self.own_property_of(&Value::Object(object), key);
        }
        if let Value::TypedArray(array) = value {
            if let Some(index) = array_index(key) {
                return Ok(
                    super::typed::read_element(&array.borrow(), index as usize).map(|element| {
                        Property {
                            slot: Slot::Data(element),
                            enumerable: true,
                            writable: true,
                            configurable: true,
                        }
                    }),
                );
            }
            if array.borrow().object.is_none() {
                return Ok(None);
            }
            return self.own_property_of(&Value::Object(super::typed_object(array)), key);
        }
        if let Value::Object(object) = value
            && let Some(proxy) = super::reflect::proxy_of(object)
        {
            return self.proxy_own_property(&proxy, key);
        }
        Ok(match value {
            Value::Object(object) => {
                let borrowed = object.borrow();
                match borrowed.property(key) {
                    Some(property) => Some(property.clone()),
                    None => match &borrowed.class {
                        ObjectClass::String(text) => string_own_property(text, key),
                        _ => None,
                    },
                }
            }
            Value::String(text) => string_own_property(text, key),
            Value::Array(array) if array.borrow().own(key).is_some() => {
                let mut property = array.borrow().own(key).cloned().expect("checked above");
                let integrity = array.borrow().integrity;
                if integrity >= super::Integrity::Sealed {
                    property.configurable = false;
                }
                if integrity == super::Integrity::Frozen && matches!(property.slot, Slot::Data(_)) {
                    property.writable = false;
                }
                Some(property)
            }
            Value::Array(array) => {
                let integrity = array.borrow().integrity;
                if key == "length" {
                    Some(Property {
                        slot: Slot::Data(Value::Number(array.borrow().len() as f64)),
                        enumerable: false,
                        writable: integrity != super::Integrity::Frozen
                            && !array.borrow().fixed_length,
                        configurable: false,
                    })
                } else if let Some(index) = array_index(key) {
                    array
                        .borrow()
                        .get(index as usize)
                        .filter(|value| !matches!(value, Value::Uninitialized))
                        .cloned()
                        .map(|value| Property {
                            slot: Slot::Data(value),
                            enumerable: true,
                            writable: integrity != super::Integrity::Frozen,
                            configurable: integrity < super::Integrity::Sealed,
                        })
                } else {
                    None
                }
            }
            Value::Window => {
                let global = self.global.borrow();
                global.accessors.get(key).cloned().or_else(|| {
                    global.values.get(key).map(|value| Property {
                        enumerable: !global.hidden.contains(key),
                        ..Property::data(value.resolve())
                    })
                })
            }
            _ => None,
        })
    }

    pub(super) fn own_property_keys(
        &mut self,
        value: &Value,
        symbols: bool,
    ) -> Result<Vec<String>, ScriptError> {
        if let Some(object) = self.wrapper_object(value) {
            return self.own_property_keys(&Value::Object(object), symbols);
        }
        if let Value::Object(object) = value
            && let Some(proxy) = super::reflect::proxy_of(object)
        {
            let keys = self.proxy_own_keys(&proxy)?;
            return Ok(keys
                .into_iter()
                .filter(|key| is_symbol_key(key) == symbols)
                .collect());
        }
        let mut keys = self.own_keys(value, symbols);
        if let Value::TypedArray(array) = value
            && array.borrow().object.is_some()
        {
            keys.extend(self.own_keys(&Value::Object(super::typed_object(array)), symbols));
        }
        Ok(keys)
    }

    fn own_keys(&self, value: &Value, symbols: bool) -> Vec<String> {
        let keep = |key: &String| !is_private_key(key) && is_symbol_key(key) == symbols;
        match value {
            Value::Object(object) => {
                let borrowed = object.borrow();
                let mut keys: Vec<String> =
                    borrowed.ordered_keys().into_iter().filter(keep).collect();
                if let ObjectClass::String(text) = &borrowed.class
                    && !symbols
                {
                    let mut indices: Vec<String> = (0..super::strings::utf16_length(text))
                        .map(|index| index.to_string())
                        .collect();
                    indices.push("length".to_string());
                    keys.splice(0..0, indices);
                }
                keys
            }
            Value::Array(array) => {
                let array = array.borrow();
                let mut indices: Vec<usize> = array
                    .iter()
                    .enumerate()
                    .filter(|(_, value)| !matches!(value, Value::Uninitialized))
                    .map(|(index, _)| index)
                    .chain(
                        array
                            .properties
                            .iter()
                            .filter_map(|(name, _)| array_index(name).map(|index| index as usize)),
                    )
                    .collect();
                indices.sort_unstable();
                let mut keys: Vec<String> = if symbols {
                    Vec::new()
                } else {
                    indices.into_iter().map(|index| index.to_string()).collect()
                };
                if !symbols {
                    keys.push("length".to_string());
                }
                keys.extend(
                    array
                        .properties
                        .iter()
                        .map(|(name, _)| name)
                        .filter(|name| array_index(name).is_none())
                        .filter(|name| keep(name))
                        .cloned(),
                );
                keys
            }
            Value::String(text) if !symbols => {
                let mut keys: Vec<String> = (0..super::strings::utf16_length(text))
                    .map(|index| index.to_string())
                    .collect();
                keys.push("length".to_string());
                keys
            }
            Value::Window if !symbols => {
                let global = self.global.borrow();
                global
                    .values
                    .keys()
                    .chain(global.accessors.keys())
                    .cloned()
                    .collect()
            }
            other if !symbols => enumerable_object_properties(other.clone())
                .map(|entries| entries.into_iter().map(|(key, _)| key).collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn descriptor_from(&mut self, value: &Value) -> Result<(Property, [bool; 6]), ScriptError> {
        if is_primitive(value) {
            return Err(self.type_error("Property description must be an object"));
        }
        let mut present = [false; 6];
        let mut read =
            |runtime: &mut Self, key: &str, index: usize| -> Result<Option<Value>, ScriptError> {
                if runtime.has_property(value, key)? {
                    present[index] = true;
                    Ok(Some(runtime.get_property(value, key)?))
                } else {
                    Ok(None)
                }
            };
        let enumerable = read(self, "enumerable", 0)?.is_some_and(|value| value.truthy());
        let configurable = read(self, "configurable", 1)?.is_some_and(|value| value.truthy());
        let data = read(self, "value", 2)?;
        let writable = read(self, "writable", 3)?.is_some_and(|value| value.truthy());
        let get = read(self, "get", 4)?;
        let set = read(self, "set", 5)?;
        for accessor in [&get, &set].into_iter().flatten() {
            if !matches!(accessor, Value::Undefined) && !is_callable(accessor) {
                return Err(self.type_error("Getter and setter must be functions"));
            }
        }
        let is_accessor = present[4] || present[5];
        if is_accessor && (present[2] || present[3]) {
            return Err(self.type_error(
                "Invalid property descriptor. Cannot both specify accessors and a value or writable attribute",
            ));
        }
        let slot = if is_accessor {
            Slot::Accessor {
                get: get.filter(|value| !matches!(value, Value::Undefined)),
                set: set.filter(|value| !matches!(value, Value::Undefined)),
            }
        } else {
            Slot::Data(data.unwrap_or(Value::Undefined))
        };
        Ok((
            Property {
                slot,
                enumerable,
                writable,
                configurable,
            },
            present,
        ))
    }

    pub(super) fn descriptor_property(
        &mut self,
        descriptor: &Value,
    ) -> Result<Property, ScriptError> {
        Ok(self.descriptor_from(descriptor)?.0)
    }

    // ValidateAndApplyPropertyDescriptor for ordinary objects
    pub(super) fn define_property_value(
        &mut self,
        target: &Value,
        key: String,
        descriptor: &Value,
    ) -> Result<(), ScriptError> {
        if let Some(object) = self.wrapper_object(target) {
            return self.define_property_value(&Value::Object(object), key, descriptor);
        }
        if let Value::Object(object) = target
            && let Some(proxy) = super::reflect::proxy_of(object)
        {
            if !self.proxy_define(&proxy, &key, descriptor)? {
                return Err(self.type_error(format!(
                    "'defineProperty' on proxy: trap returned falsish for property '{key}'"
                )));
            }
            return Ok(());
        }
        let (mut property, present) = self.descriptor_from(descriptor)?;
        let object = match target {
            Value::Object(object) => object.clone(),
            Value::TypedArray(array) => match super::typed::canonical_numeric_index(&key) {
                Some(number) => {
                    let index = super::typed::valid_index(&array.borrow(), number);
                    // an element is always a writable, enumerable and configurable value
                    let refused = (present[1] && !property.configurable)
                        || (present[0] && !property.enumerable)
                        || present[4]
                        || present[5]
                        || (present[3] && !property.writable);
                    let Some(index) = index.filter(|_| !refused) else {
                        return Err(self.type_error(format!("Cannot redefine property: {key}")));
                    };
                    if let (true, Slot::Data(value)) = (present[2], property.slot) {
                        return self.typed_set(array, index, value);
                    }
                    return Ok(());
                }
                None => super::typed_object(array),
            },
            Value::Array(array) => {
                return self.define_array_property(array, key, property, present);
            }
            Value::Window => {
                let existing = self.global.borrow().accessors.get(&key).cloned();
                if let Some(current) = existing {
                    if !current.configurable {
                        return Err(self.type_error(format!("Cannot redefine property: {key}")));
                    }
                    if let (
                        Slot::Accessor { get, set },
                        Slot::Accessor {
                            get: new_get,
                            set: new_set,
                        },
                    ) = (current.slot, &mut property.slot)
                    {
                        if !present[4] {
                            *new_get = get;
                        }
                        if !present[5] {
                            *new_set = set;
                        }
                        if !present[0] {
                            property.enumerable = current.enumerable;
                        }
                        if !present[1] {
                            property.configurable = current.configurable;
                        }
                    }
                }
                let mut global = self.global.borrow_mut();
                match property.slot {
                    Slot::Data(value)
                        if property.writable && property.enumerable && property.configurable =>
                    {
                        global.accessors.remove(&key);
                        global.hidden.remove(&key);
                        global.values.insert(key, value);
                    }
                    _ => {
                        global.values.remove(&key);
                        global.accessors.insert(key, property);
                    }
                }
                return Ok(());
            }
            _ => return Err(self.type_error("Object.defineProperty called on non-object")),
        };
        let existing = object.borrow().property(&key).cloned();
        match existing {
            None => {
                if !object.borrow().extensible {
                    return Err(self.type_error(format!(
                        "Cannot define property {key}, object is not extensible"
                    )));
                }
                object.borrow_mut().define(key, property);
            }
            Some(current) => {
                if !current.configurable {
                    let changes_kind = (present[4] || present[5])
                        != matches!(current.slot, Slot::Accessor { .. })
                        && (present[2] || present[3] || present[4] || present[5]);
                    let invalid = (present[1] && property.configurable)
                        || (present[0] && property.enumerable != current.enumerable)
                        || changes_kind
                        || match (&current.slot, &property.slot) {
                            (Slot::Data(old), Slot::Data(new)) => {
                                !current.writable
                                    && ((present[3] && property.writable)
                                        || (present[2] && !same_value(old, new)))
                            }
                            (
                                Slot::Accessor { get, set },
                                Slot::Accessor {
                                    get: new_get,
                                    set: new_set,
                                },
                            ) => {
                                (present[4] && !same_optional(get, new_get))
                                    || (present[5] && !same_optional(set, new_set))
                            }
                            _ => false,
                        };
                    if invalid {
                        return Err(self.type_error(format!("Cannot redefine property: {key}")));
                    }
                }
                if !present[0] {
                    property.enumerable = current.enumerable;
                }
                if !present[1] {
                    property.configurable = current.configurable;
                }
                match (&current.slot, &mut property.slot) {
                    (Slot::Data(old), Slot::Data(new)) if !(present[4] || present[5]) => {
                        if !present[2] {
                            *new = old.clone();
                        }
                        if !present[3] {
                            property.writable = current.writable;
                        }
                    }
                    (
                        Slot::Accessor { get, set },
                        Slot::Accessor {
                            get: new_get,
                            set: new_set,
                        },
                    ) => {
                        if !present[4] {
                            *new_get = get.clone();
                        }
                        if !present[5] {
                            *new_set = set.clone();
                        }
                    }
                    (Slot::Accessor { .. }, Slot::Data(_)) if !(present[2] || present[3]) => {
                        property.slot = current.slot.clone();
                    }
                    _ => {}
                }
                object.borrow_mut().define(key, property);
            }
        }
        Ok(())
    }

    fn define_properties(&mut self, target: &Value, properties: &Value) -> Result<(), ScriptError> {
        let entries = self.own_enumerable_entries(properties)?;
        for (key, descriptor) in entries {
            self.define_property_value(target, key, &descriptor)?;
        }
        Ok(())
    }

    fn function_source(&self, value: &Value) -> String {
        if let Value::Object(object) = value {
            let borrowed = object.borrow();
            match &borrowed.class {
                ObjectClass::Function(closure) => {
                    if let Some((source, start, end)) = &closure.code.source {
                        return source[*start..*end].to_string();
                    }
                }
                ObjectClass::Native(_) | ObjectClass::Bound { .. } => {
                    let name = match borrowed.get("name") {
                        Some(Value::String(name)) => name.to_string(),
                        _ => String::new(),
                    };
                    return format!("function {name}() {{ [native code] }}");
                }
                _ => {}
            }
        }
        "function () { [native code] }".to_string()
    }

    fn array_like_values(&mut self, value: &Value) -> Result<Vec<Value>, ScriptError> {
        match value {
            Value::Undefined | Value::Null => Ok(Vec::new()),
            Value::Array(array) => Ok(array.borrow().to_vec()),
            Value::Object(_) => self.array_like_list(value),
            _ => Err(self.type_error("CreateListFromArrayLike called on non-object")),
        }
    }

    fn this_number(&mut self, this: &Value, method: &str) -> Result<f64, ScriptError> {
        match this {
            Value::Number(number) => Ok(*number),
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Number(number) => Ok(*number),
                _ => Err(runtime_error(format!(
                    "Number.prototype.{method} requires that 'this' be a Number"
                ))),
            },
            _ => Err(runtime_error(format!(
                "Number.prototype.{method} requires that 'this' be a Number"
            ))),
        }
        .map_err(|error| self.type_error(error.message))
    }

    fn this_symbol(&mut self, this: &Value) -> Result<String, ScriptError> {
        match this {
            Value::Symbol(key) => Ok(key.clone()),
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Symbol(key) => Ok(key.clone()),
                _ => Err(runtime_error("not a symbol")),
            },
            _ => Err(runtime_error("not a symbol")),
        }
        .map_err(|_| self.type_error("Symbol.prototype method requires that 'this' be a Symbol"))
    }

    fn generator_state(
        &mut self,
        this: &Value,
    ) -> Result<Rc<RefCell<vm::GeneratorState>>, ScriptError> {
        if let Value::Object(object) = this
            && let ObjectClass::Generator(state) = &object.borrow().class
        {
            return Ok(state.clone());
        }
        Err(self.type_error("next method called on incompatible receiver"))
    }

    pub(super) fn call_native(
        &mut self,
        native: Native,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let first = argument(&arguments, 0);
        match native {
            Native::ObjectConstructor => {
                self.construct_native(Native::ObjectConstructor, arguments, Value::Undefined)
            }
            Native::ObjectCreate => {
                let prototype = match &first {
                    Value::Object(prototype) => Some(prototype.clone()),
                    Value::Null => None,
                    _ => {
                        return Err(
                            self.type_error("Object prototype may only be an Object or null")
                        );
                    }
                };
                let object = Value::Object(new_object(JsObject::with_prototype(
                    prototype,
                    ObjectClass::Ordinary,
                )));
                let properties = argument(&arguments, 1);
                if !matches!(properties, Value::Undefined) {
                    self.define_properties(&object, &properties)?;
                }
                Ok(object)
            }
            Native::ObjectDefineProperty => {
                if is_primitive(&first) {
                    return Err(self.type_error("Object.defineProperty called on non-object"));
                }
                let key = self.coerce_key(argument(&arguments, 1))?;
                self.define_property_value(&first, key, &argument(&arguments, 2))?;
                Ok(first)
            }
            Native::ObjectDefineProperties => {
                if is_primitive(&first) {
                    return Err(self.type_error("Object.defineProperties called on non-object"));
                }
                self.define_properties(&first, &argument(&arguments, 1))?;
                Ok(first)
            }
            Native::ObjectGetPrototypeOf => {
                if matches!(first, Value::Undefined | Value::Null) {
                    return Err(self.type_error("Cannot convert undefined or null to object"));
                }
                Ok(self
                    .prototype_of_value(&first)?
                    .map(Value::Object)
                    .unwrap_or(Value::Null))
            }
            Native::ObjectSetPrototypeOf => {
                if matches!(first, Value::Undefined | Value::Null) {
                    return Err(
                        self.type_error("Object.setPrototypeOf called on null or undefined")
                    );
                }
                let prototype = match argument(&arguments, 1) {
                    Value::Object(prototype) => Some(prototype),
                    Value::Null => None,
                    _ => {
                        return Err(
                            self.type_error("Object prototype may only be an Object or null")
                        );
                    }
                };
                if let Value::Array(array) = &first {
                    if !self.set_array_prototype(array, prototype) {
                        return Err(self.type_error("#<Array> is not extensible"));
                    }
                    return Ok(first);
                }
                let target = self.wrapper_object(&first).map(Value::Object);
                if let Value::Object(object) = target.as_ref().unwrap_or(&first) {
                    self.set_object_prototype(object, prototype)?;
                }
                Ok(first)
            }
            Native::ObjectGetOwnPropertyNames | Native::ObjectGetOwnPropertySymbols => {
                let symbols = matches!(native, Native::ObjectGetOwnPropertySymbols);
                let target = self.coerce_object(first)?;
                let keys = self
                    .own_property_keys(&target, symbols)?
                    .into_iter()
                    .map(|key| {
                        if symbols {
                            Value::Symbol(key)
                        } else {
                            Value::String(key.into())
                        }
                    })
                    .collect();
                Ok(Self::new_array(keys))
            }
            Native::ObjectGetOwnPropertyDescriptor => {
                let target = self.coerce_object(first)?;
                let key = self.coerce_key(argument(&arguments, 1))?;
                Ok(match self.own_property_of(&target, &key)? {
                    Some(property) => self.descriptor_value(&property),
                    None => Value::Undefined,
                })
            }
            Native::ObjectGetOwnPropertyDescriptors => {
                let target = self.coerce_object(first)?;
                let result = self.new_plain_object();
                let mut keys = self.own_property_keys(&target, false)?;
                keys.extend(self.own_property_keys(&target, true)?);
                for key in keys {
                    if let Some(property) = self.own_property_of(&target, &key)? {
                        let descriptor = self.descriptor_value(&property);
                        result.borrow_mut().insert(key, descriptor);
                    }
                }
                Ok(Value::Object(result))
            }
            Native::ObjectKeys => {
                let target = self.coerce_object(first)?;
                let keys = self.own_enumerable_keys(&target)?;
                Ok(Self::new_array(
                    keys.into_iter()
                        .map(|key| Value::String(key.into()))
                        .collect(),
                ))
            }
            Native::ObjectValues | Native::ObjectEntries => {
                let target = self.coerce_object(first)?;
                let entries: Vec<(String, Value)> = self
                    .own_enumerable_entries(&target)?
                    .into_iter()
                    .filter(|(key, _)| !is_symbol_key(key))
                    .collect();
                let values = entries
                    .into_iter()
                    .map(|(key, value)| match native {
                        Native::ObjectValues => value,
                        _ => Self::new_array(vec![Value::String(key.into()), value]),
                    })
                    .collect();
                Ok(Self::new_array(values))
            }
            Native::ObjectAssign => {
                let target = self.coerce_object(first)?;
                for source in arguments.into_iter().skip(1) {
                    if matches!(source, Value::Undefined | Value::Null) {
                        continue;
                    }
                    for (key, value) in self.own_enumerable_entries(&source)? {
                        self.set_property(&target, &key, value, true)?;
                    }
                }
                Ok(target)
            }
            Native::ObjectFromEntries => {
                let result = self.new_plain_object();
                self.for_each_iterated(first, |runtime, entry| {
                    if is_primitive(&entry) {
                        return Err(runtime.type_error("Iterator value is not an entry object"));
                    }
                    let key = runtime.get_index(&entry, Value::Number(0.0))?;
                    let key = runtime.coerce_key(key)?;
                    let value = runtime.get_index(&entry, Value::Number(1.0))?;
                    result.borrow_mut().define(key, Property::data(value));
                    Ok(())
                })?;
                Ok(Value::Object(result))
            }
            Native::ObjectIs => Ok(Value::Bool(same_value(&first, &argument(&arguments, 1)))),
            Native::ObjectFreeze | Native::ObjectSeal | Native::ObjectPreventExtensions => {
                let target = match &first {
                    Value::TypedArray(array) => {
                        if !matches!(native, Native::ObjectPreventExtensions)
                            && super::typed::element_count(&array.borrow()) > 0
                        {
                            return Err(self.type_error(
                                "Cannot freeze or seal an array buffer view with elements",
                            ));
                        }
                        Some(super::typed_object(array))
                    }
                    Value::Object(object) => Some(object.clone()),
                    Value::Array(array) => {
                        let wanted = match native {
                            Native::ObjectFreeze => super::Integrity::Frozen,
                            Native::ObjectSeal => super::Integrity::Sealed,
                            _ => super::Integrity::Closed,
                        };
                        let mut array = array.borrow_mut();
                        array.integrity = array.integrity.max(wanted);
                        None
                    }
                    _ => None,
                };
                if let Some(object) = &target {
                    let mut object = object.borrow_mut();
                    match native {
                        Native::ObjectFreeze => object.freeze(false),
                        Native::ObjectSeal => object.freeze(true),
                        _ => object.extensible = false,
                    }
                }
                Ok(first)
            }
            Native::ObjectIsFrozen | Native::ObjectIsSealed => {
                let frozen = matches!(native, Native::ObjectIsFrozen);
                Ok(Value::Bool(match &first {
                    Value::Object(object) => {
                        let object = object.borrow();
                        !object.extensible
                            && object.entries().iter().all(|(_, property)| {
                                !property.configurable
                                    && (!frozen
                                        || !property.writable
                                        || matches!(property.slot, Slot::Accessor { .. }))
                            })
                    }
                    Value::Array(array) => {
                        let array = array.borrow();
                        let empty = array
                            .iter()
                            .all(|value| matches!(value, Value::Uninitialized));
                        let wanted = if frozen {
                            super::Integrity::Frozen
                        } else {
                            super::Integrity::Sealed
                        };
                        array.integrity >= wanted
                            || (array.integrity == super::Integrity::Closed && empty)
                    }
                    value => is_primitive(value),
                }))
            }
            Native::ObjectIsExtensible => Ok(Value::Bool(match &first {
                Value::Array(array) => array.borrow().integrity == super::Integrity::Open,
                Value::Object(object) => object.borrow().extensible,
                Value::TypedArray(array) => array
                    .borrow()
                    .object
                    .as_ref()
                    .is_none_or(|object| object.borrow().extensible),
                value => !is_primitive(value),
            })),
            Native::ObjectHasOwn => {
                let target = self.coerce_object(first)?;
                let key = self.coerce_key(argument(&arguments, 1))?;
                Ok(Value::Bool(self.own_property_of(&target, &key)?.is_some()))
            }
            Native::ObjectHasOwnProperty => {
                let key = self.coerce_key(first)?;
                let target = self.coerce_object(this)?;
                Ok(Value::Bool(self.own_property_of(&target, &key)?.is_some()))
            }
            Native::ObjectIsPrototypeOf => {
                let Value::Object(prototype) = this else {
                    return Ok(Value::Bool(false));
                };
                let mut current = self.prototype_of(&first);
                while let Some(object) = current {
                    if Rc::ptr_eq(&object, &prototype) {
                        return Ok(Value::Bool(true));
                    }
                    current = object.borrow().prototype_or(&self.intrinsics);
                }
                Ok(Value::Bool(false))
            }
            Native::ObjectPropertyIsEnumerable => {
                let key = self.coerce_key(first)?;
                let target = self.coerce_object(this)?;
                Ok(Value::Bool(
                    self.own_property_of(&target, &key)?
                        .is_some_and(|property| property.enumerable),
                ))
            }
            Native::ObjectToString => Ok(Value::String(self.object_to_string(&this)?.into())),
            Native::ObjectToLocaleString => {
                let method = self.get_property(&this, "toString")?;
                self.call_function(method, this, Vec::new())
            }
            Native::ObjectValueOf => self.coerce_object(this),
            Native::ObjectProtoGetter => {
                let target = self.coerce_object(this)?;
                Ok(self
                    .prototype_of(&target)
                    .map(Value::Object)
                    .unwrap_or(Value::Null))
            }
            Native::ObjectProtoSetter => {
                if let Value::Object(object) = &this {
                    match first {
                        Value::Object(prototype) => {
                            self.set_object_prototype(object, Some(prototype))?
                        }
                        Value::Null => self.set_object_prototype(object, None)?,
                        _ => {}
                    }
                }
                Ok(Value::Undefined)
            }
            Native::ObjectDefineGetter | Native::ObjectDefineSetter => {
                let getter = matches!(native, Native::ObjectDefineGetter);
                let target = self.coerce_object(this)?;
                let accessor = argument(&arguments, 1);
                if !is_callable(&accessor) {
                    return Err(self.type_error(if getter {
                        "Getter must be a function"
                    } else {
                        "Setter must be a function"
                    }));
                }
                let key = self.coerce_key(argument(&arguments, 0))?;
                let descriptor = self.new_plain_object();
                {
                    let mut object = descriptor.borrow_mut();
                    object.insert(
                        if getter { "get" } else { "set" }.into(),
                        accessor,
                    );
                    object.insert("enumerable".into(), Value::Bool(true));
                    object.insert("configurable".into(), Value::Bool(true));
                }
                self.define_property_value(&target, key, &Value::Object(descriptor))?;
                Ok(Value::Undefined)
            }
            Native::ObjectLookupGetter | Native::ObjectLookupSetter => {
                let getter = matches!(native, Native::ObjectLookupGetter);
                let mut target = self.coerce_object(this)?;
                let key = self.coerce_key(argument(&arguments, 0))?;
                loop {
                    if let Some(property) = self.own_property_of(&target, &key)? {
                        return Ok(match property.slot {
                            Slot::Accessor { get, .. } if getter => {
                                get.unwrap_or(Value::Undefined)
                            }
                            Slot::Accessor { set, .. } => set.unwrap_or(Value::Undefined),
                            _ => Value::Undefined,
                        });
                    }
                    target = match self.prototype_of_value(&target)? {
                        Some(prototype) => Value::Object(prototype),
                        None => return Ok(Value::Undefined),
                    };
                }
            }
            Native::FunctionConstructor => {
                self.construct_native(Native::FunctionConstructor, arguments, Value::Undefined)
            }
            Native::FunctionPrototype => Ok(Value::Undefined),
            Native::FunctionCall => {
                if !is_callable(&this) {
                    return Err(self.type_error("Function.prototype.call called on non-function"));
                }
                let mut arguments = arguments.into_iter();
                let receiver = arguments.next().unwrap_or(Value::Undefined);
                self.call_function(this, receiver, arguments.collect())
            }
            Native::FunctionApply => {
                if !is_callable(&this) {
                    return Err(
                        self.type_error("Function.prototype.apply was called on a non-function")
                    );
                }
                let list = self.array_like_values(&argument(&arguments, 1))?;
                self.call_function(this, first, list)
            }
            Native::FunctionBind => {
                if !is_callable(&this) {
                    return Err(self.type_error("Bind must be called on a function"));
                }
                let bound: Vec<Value> = arguments.iter().skip(1).cloned().collect();
                let target_length = match self.get_property(&this, "length")? {
                    Value::Number(length) if length.is_finite() => length.max(0.0),
                    _ => 0.0,
                };
                let target_name = match self.get_property(&this, "name")? {
                    Value::String(name) => name.to_string(),
                    _ => String::new(),
                };
                let length = (target_length - bound.len() as f64).max(0.0);
                let prototype = self.prototype_of(&this);
                let mut object = JsObject::with_prototype(
                    prototype,
                    ObjectClass::Bound {
                        target: this,
                        this: first,
                        arguments: bound,
                    },
                );
                object.define(
                    "length".to_string(),
                    Property {
                        writable: false,
                        ..Property::hidden(Value::Number(length))
                    },
                );
                object.define(
                    "name".to_string(),
                    Property {
                        writable: false,
                        ..Property::hidden(Value::String(format!("bound {target_name}").into()))
                    },
                );
                Ok(Value::Object(new_object(object)))
            }
            Native::FunctionToString => {
                if !is_callable(&this) {
                    return Err(self.type_error(
                        "Function.prototype.toString requires that 'this' be a Function",
                    ));
                }
                Ok(Value::String(self.function_source(&this).into()))
            }
            Native::FunctionHasInstance => {
                Ok(Value::Bool(self.ordinary_has_instance(&this, &first)?))
            }
            Native::SymbolConstructor => {
                self.symbol_counter += 1;
                let key = match first {
                    Value::Undefined => format!("{SYMBOL_PREFIX}{}", self.symbol_counter),
                    other => {
                        let description = self.coerce_string(other)?;
                        format!("{SYMBOL_PREFIX}{}:{description}", self.symbol_counter)
                    }
                };
                Ok(Value::Symbol(key))
            }
            Native::SymbolFor => {
                let description = self.coerce_string(first)?;
                let key = format!("{SYMBOL_PREFIX}for:{description}");
                Ok(Value::Symbol(key))
            }
            Native::SymbolKeyFor => match first {
                Value::Symbol(key) => Ok(key
                    .strip_prefix(SYMBOL_PREFIX)
                    .and_then(|rest| rest.strip_prefix("for:"))
                    .map(|description| Value::String(description.to_string().into()))
                    .unwrap_or(Value::Undefined)),
                _ => Err(self.type_error("Symbol.keyFor requires a symbol")),
            },
            Native::SymbolToString => {
                let key = self.this_symbol(&this)?;
                Ok(Value::String(
                    format!("Symbol({})", symbol_description(&key).unwrap_or_default()).into(),
                ))
            }
            Native::SymbolValueOf => Ok(Value::Symbol(self.this_symbol(&this)?)),
            Native::SymbolDescription => {
                let key = self.this_symbol(&this)?;
                Ok(symbol_description(&key)
                    .map(|text| Value::String(text.into()))
                    .unwrap_or(Value::Undefined))
            }
            Native::ErrorConstructor(kind) => {
                let constructor = Value::Undefined;
                self.construct_error_object(kind, arguments, &constructor)
            }
            Native::ErrorToString => {
                if is_primitive(&this) {
                    return Err(self
                        .type_error("Error.prototype.toString requires that 'this' be an Object"));
                }
                let name = match self.get_property(&this, "name")? {
                    Value::Undefined => "Error".to_string(),
                    name => self.coerce_string(name)?,
                };
                let message = match self.get_property(&this, "message")? {
                    Value::Undefined => String::new(),
                    message => self.coerce_string(message)?,
                };
                let text = match (name.is_empty(), message.is_empty()) {
                    (_, true) => name,
                    (true, false) => message,
                    (false, false) => format!("{name}: {message}"),
                };
                Ok(Value::String(text.into()))
            }
            Native::ErrorIsError => Ok(Value::Bool(match first.resolve() {
                Value::Error(_) => true,
                Value::Object(object) => matches!(object.borrow().class, ObjectClass::Error),
                _ => false,
            })),
            Native::RegExpEscape => {
                let Value::String(text) = first.resolve() else {
                    return Err(self.type_error("RegExp.escape requires a string"));
                };
                Ok(Value::String(regexp_escape(&text).into()))
            }
            Native::ErrorCaptureStackTrace => {
                if let Value::Object(object) = &first {
                    let mut text = self.coerce_string(first.clone())?;
                    self.write_stack(&mut text);
                    object.borrow_mut().define(
                        "stack".to_string(),
                        Property::hidden(Value::String(text.into())),
                    );
                }
                Ok(Value::Undefined)
            }
            Native::BooleanConstructor => Ok(Value::Bool(first.truthy())),
            Native::BooleanToString | Native::BooleanValueOf => {
                let value = match &this {
                    Value::Bool(value) => Some(*value),
                    Value::Object(object) => match &object.borrow().class {
                        ObjectClass::Boolean(value) => Some(*value),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(value) = value else {
                    return Err(self
                        .type_error("Boolean.prototype method requires that 'this' be a Boolean"));
                };
                Ok(if matches!(native, Native::BooleanToString) {
                    Value::String(value.to_string().into())
                } else {
                    Value::Bool(value)
                })
            }
            Native::NumberConstructor => Ok(Value::Number(if arguments.is_empty() {
                0.0
            } else {
                match self.coerce_numeric(first)? {
                    Value::BigInt(value) => value.to_f64(),
                    numeric => self.coerce_number(numeric)?,
                }
            })),
            Native::NumberToString => {
                let number = self.this_number(&this, "toString")?;
                let radix = match first {
                    Value::Undefined => 10.0,
                    radix => self.coerce_number(radix)?.trunc(),
                };
                if !(2.0..=36.0).contains(&radix) {
                    return Err(self.range_error("toString() radix must be between 2 and 36"));
                }
                let text = if radix == 10.0 {
                    number_to_string(number)
                } else {
                    number_to_radix(number, radix as u32)
                };
                Ok(Value::String(text.into()))
            }
            Native::NumberValueOf => Ok(Value::Number(self.this_number(&this, "valueOf")?)),
            Native::NumberIsInteger => Ok(Value::Bool(
                matches!(first, Value::Number(number) if number.is_finite() && number.trunc() == number),
            )),
            Native::NumberIsSafeInteger => Ok(Value::Bool(matches!(first, Value::Number(number)
                if number.is_finite() && number.trunc() == number && number.abs() <= 9_007_199_254_740_991.0))),
            Native::NumberIsNaN => Ok(Value::Bool(
                matches!(first, Value::Number(number) if number.is_nan()),
            )),
            Native::StringConstructor => {
                let text = match first {
                    Value::Undefined if arguments.is_empty() => String::new(),
                    Value::Symbol(key) => {
                        format!("Symbol({})", symbol_description(&key).unwrap_or_default())
                    }
                    other => self.coerce_string(other)?,
                };
                Ok(Value::String(text.into()))
            }
            Native::StringToString => {
                match &this {
                    Value::String(text) => Ok(Value::String(text.clone())),
                    Value::Object(object) => match &object.borrow().class {
                        ObjectClass::String(text) => Ok(Value::String(text.clone().into())),
                        _ => Err(runtime_error(
                            "String.prototype.toString requires that 'this' be a String",
                        )),
                    }
                    .map_err(|error| self.type_error(error.message)),
                    _ => Err(self
                        .type_error("String.prototype.toString requires that 'this' be a String")),
                }
            }
            Native::StringFromCharCode => {
                let mut units = Vec::with_capacity(arguments.len());
                for value in arguments {
                    units.push(to_uint32_from(self.coerce_number(value)?) as u16);
                }
                Ok(Value::String(String::from_utf16_lossy(&units).into()))
            }
            Native::ArrayConstructor => {
                self.construct_native(Native::ArrayConstructor, arguments, Value::Undefined)
            }
            Native::ArrayFrom => {
                let mapper = argument(&arguments, 1);
                if !matches!(mapper, Value::Undefined) && !is_callable(&mapper) {
                    return Err(self.type_error("Array.from mapper is not a function"));
                }
                let iterable = match &first {
                    Value::Undefined | Value::Null => {
                        return Err(
                            self.type_error("Array.from requires an array-like or iterable object")
                        );
                    }
                    Value::Object(_) => !matches!(
                        self.get_property(&first, "@@iterator")?,
                        Value::Undefined | Value::Null
                    ),
                    _ => true,
                };
                let values = if iterable {
                    self.iterate_to_vec(first)?
                } else {
                    self.array_like_values(&first)?
                };
                if matches!(mapper, Value::Undefined) {
                    return Ok(Self::new_array(values));
                }
                let receiver = argument(&arguments, 2);
                let mut mapped = Vec::with_capacity(values.len());
                for (index, value) in values.into_iter().enumerate() {
                    mapped.push(self.call_function(
                        mapper.clone(),
                        receiver.clone(),
                        vec![value, Value::Number(index as f64)],
                    )?);
                }
                Ok(Self::new_array(mapped))
            }
            Native::ArrayOf => Ok(Self::new_array(arguments)),
            Native::ArrayValues | Native::ArrayKeys | Native::ArrayEntries => {
                if matches!(this, Value::Undefined | Value::Null) {
                    return Err(
                        self.type_error("Array.prototype.values called on null or undefined")
                    );
                }
                let kind = match native {
                    Native::ArrayValues => ArrayIterationKind::Values,
                    Native::ArrayKeys => ArrayIterationKind::Keys,
                    _ => ArrayIterationKind::Entries,
                };
                let iteration = ArrayIteration {
                    target: this,
                    index: 0,
                    kind,
                    done: false,
                };
                Ok(Value::Object(new_object(JsObject::with_prototype(
                    Some(self.intrinsics.array_iterator_prototype.clone()),
                    ObjectClass::ArrayIterator(Rc::new(RefCell::new(iteration))),
                ))))
            }
            Native::ArrayIteratorNext => {
                let iteration = match &this {
                    Value::Object(object) => match &object.borrow().class {
                        ObjectClass::ArrayIterator(iteration) => Some(iteration.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(iteration) = iteration else {
                    return Err(
                        self.type_error("next called on an object that is not an Array Iterator")
                    );
                };
                let (target, index, kind, done) = {
                    let state = iteration.borrow();
                    (state.target.clone(), state.index, state.kind, state.done)
                };
                if done {
                    return Ok(self.iterator_result(Value::Undefined, true));
                }
                // the length is read on every step, so the iterator sees pushes and pops
                let length = match &target {
                    Value::Array(array) => array.borrow().len(),
                    other => {
                        let length = self.get_property(other, "length")?;
                        let length = self.coerce_number(length)?;
                        if length.is_nan() || length <= 0.0 {
                            0
                        } else {
                            length.min(9_007_199_254_740_991.0) as usize
                        }
                    }
                };
                if index >= length {
                    iteration.borrow_mut().done = true;
                    return Ok(self.iterator_result(Value::Undefined, true));
                }
                iteration.borrow_mut().index = index + 1;
                let key = Value::Number(index as f64);
                let value = match kind {
                    ArrayIterationKind::Keys => key,
                    ArrayIterationKind::Values => self.get_index(&target, key)?,
                    ArrayIterationKind::Entries => {
                        let element = self.get_index(&target, key.clone())?;
                        Self::new_array(vec![key, element])
                    }
                };
                Ok(self.iterator_result(value, false))
            }
            Native::IteratorSelf => Ok(this),
            Native::GeneratorNext | Native::GeneratorReturn | Native::GeneratorThrow => {
                let state = self.generator_state(&this)?;
                let mode = match native {
                    Native::GeneratorNext => ResumeMode::Next,
                    Native::GeneratorReturn => ResumeMode::Return,
                    _ => ResumeMode::Throw,
                };
                self.generator_resume(&state, mode, first)
            }
            Native::AsyncGeneratorNext
            | Native::AsyncGeneratorReturn
            | Native::AsyncGeneratorThrow => {
                let data = match &this {
                    Value::Object(object) => match &object.borrow().class {
                        ObjectClass::AsyncGenerator(data) => Some(data.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(data) = data else {
                    let error =
                        self.make_error("TypeError", "next method called on incompatible receiver");
                    return Ok(Value::Promise(self.rejected_promise_value(error)));
                };
                let mode = match native {
                    Native::AsyncGeneratorNext => ResumeMode::Next,
                    Native::AsyncGeneratorReturn => ResumeMode::Return,
                    _ => ResumeMode::Throw,
                };
                self.async_generator_enqueue(&data, mode, first)
            }
            Native::AsyncResume(handle, rejected) => {
                self.async_resume(&handle.0, first, rejected)?;
                Ok(Value::Undefined)
            }
            Native::IterResultWrap(done) => Ok(self.iterator_result(first, done)),
            // an index nothing was put at leaves a method as undefined
            Native::Array(method) => self
                .call_array_method(method, this, arguments)
                .map(Value::filled),
            Native::String(method) => self.call_string_method(method, this, arguments),
            Native::Math(function) => self.call_math(function, arguments),
            Native::NumberFormat(super::numbers::NumberFormat::Locale) => {
                let number = self.this_number(&this, "toLocaleString")?;
                let options = self.number_options(&first, &argument(&arguments, 1))?;
                Ok(Value::String(
                    super::intl::format_number(&options, number).into(),
                ))
            }
            Native::NumberFormat(format) => {
                let number = self.this_number(&this, "toFixed")?;
                self.format_number(format, number, first)
            }
            Native::RegExp(method) => self.call_regexp(method, this, arguments, None),
            Native::Console(method) => self.call_console(method, arguments),
            Native::Promise(function) => {
                self.call_promise_function((*function).clone(), this, arguments)
            }
            Native::Dom(member) => self.call_dom(member, this, arguments),
            Native::DomConstructor(name)
            | Native::EventConstructor(name)
            | Native::RangeConstructor(name) => Err(self.type_error(format!(
                "Failed to construct '{name}': Please use the 'new' operator"
            ))),
            Native::Event(member) => self.call_event_member(member, this, arguments),
            Native::Range(member) => self.call_range_member(member, this, arguments),
            Native::Window(method) => self.call_window_method(method, this, arguments),
            Native::Observer(method) => self.call_observer_method(method, this, arguments),
            Native::Xhr(member) => self.call_xhr(member, this, arguments),
            Native::Socket(member) => self.call_socket(member, this, arguments),
            Native::Notice(member) => self.call_notice(member, this, arguments),
            Native::Channel(member) => self.call_channel(member, this, arguments),
            Native::ChannelDeliver => self.deliver_broadcasts(),
            Native::Reader(member) => self.call_reader(member, this, arguments),
            Native::ReaderDeliver(id, generation) => self.deliver_reader(id, generation),
            Native::Port(member) => self.call_port(member, this, arguments),
            Native::Walk(member) => self.call_walk(member, this, arguments),
            Native::Helper(member) => self.call_iterator_helper(member, this, arguments),
            Native::Markup(member) => self.call_markup(member, this, arguments),
            Native::Path(member) => self.call_path(member, this, arguments),
            Native::Matrix(member) => self.call_matrix(member, this, arguments),
            Native::Segmenter(member) => self.call_segmenter(member, this, arguments),
            Native::Disposable(member, asynchronous) => {
                self.call_disposable(member, asynchronous, this, arguments)
            }
            Native::Atomics(op) => self.call_atomics(op, arguments),
            Native::Locale(member) => self.call_locale(member, this, arguments),
            Native::DisplayNames(member) => self.call_display_names(member, this, arguments),
            Native::DurationFormat(member) => self.call_duration_format(member, this, arguments),
            Native::Offscreen(member) => self.call_offscreen(member, this, arguments),
            Native::UrlPattern(member) => self.call_url_pattern(member, this, arguments),
            Native::XPath(member) => self.call_xpath(member, this, arguments),
            Native::StyleOm(member) => self.call_style_om(member, this, arguments),
            Native::Temporal(member) => self.call_temporal(member, this, arguments),
            Native::PerfObserver(member) => self.call_perf_observer(member, this, arguments),
            Native::PerfDeliver(observer) => self.deliver_perf_entries(&observer),
            Native::ViewTransition(transition, which) => {
                self.step_view_transition(&transition, which, first)
            }
            Native::Cssom(member) => self.call_cssom(member, this, arguments),
            Native::Font(member) => self.call_font(member, this, arguments),
            Native::MatrixConstructor(_) => Err(self.type_error(
                "Failed to construct 'DOMMatrix': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
            )),
            Native::Stream(function) => self.call_stream((*function).clone(), this, arguments),
            Native::Nav(function) => self.call_nav((*function).clone(), this, arguments),
            Native::Anim(member) => self.call_animation(member, this, arguments),
            Native::AnimFinish(id, generation) => self.deliver_animation_finish(id, generation),
            Native::Shape(member) => self.call_shape(member, this, arguments),
            Native::PortDeliver => self.deliver_port_messages(),
            Native::Url(member) => self.call_url(member, this, arguments),
            Native::UrlSearchParams(member) => self.call_url_search_params(member, this, arguments),
            Native::UrlConstructor => {
                Err(self.type_error("Failed to construct 'URL': Please use the 'new' operator"))
            }
            Native::UrlSearchParamsConstructor => Err(self.type_error(
                "Failed to construct 'URLSearchParams': Please use the 'new' operator",
            )),
            Native::CustomElements(method) => self.call_registry(method, arguments),
            Native::XhrDeliver(id, generation) => self.deliver_xhr(id, generation),
            Native::ImageDeliver(node, generation) => self.deliver_image(node, generation),
            Native::ScriptElement(node) => self.run_script_element(node),
            Native::DialogClose(node) => self.deliver_dialog_close(node),
            Native::MediaLoad(node, generation) => self.deliver_media_load(node, generation),
            Native::MediaCue(cue) => {
                let (node, generation, cue) = *cue;
                self.deliver_media_cue(node, generation, cue)
            }
            Native::Audio(member) => self.call_audio(member, this, arguments),
            Native::AudioStateChange(id) => self.deliver_audio_state(id),
            Native::AudioConstructor(class) => Err(self.type_error(format!(
                "Failed to construct '{}': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
                super::audio::class_name(class)
            ))),
            Native::XhrConstructor => Err(self
                .type_error("Failed to construct 'XMLHttpRequest': Please use the 'new' operator")),
            Native::SocketConstructor => Err(self
                .type_error("Failed to construct 'WebSocket': Please use the 'new' operator")),
            Native::NoticeConstructor => Err(self.type_error(
                "Failed to construct 'Notification': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
            )),
            Native::ChannelConstructor => Err(self.type_error(
                "Failed to construct 'BroadcastChannel': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
            )),
            Native::ReaderConstructor => Err(self.type_error(
                "Failed to construct 'FileReader': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
            )),
            Native::MessageChannelConstructor => Err(self.type_error(
                "Failed to construct 'MessageChannel': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
            )),
            Native::DataTransferConstructor => {
                Err(self.type_error("Constructor DataTransfer requires 'new'"))
            }
            Native::QuadConstructor => Err(self.type_error("Constructor DOMQuad requires 'new'")),
            Native::PortConstructor | Native::IllegalConstructor => {
                Err(self.type_error("Illegal constructor"))
            }
            Native::AnimConstructor => Err(self.type_error("Illegal constructor")),
            Native::ClipboardItemConstructor => Err(self.type_error(
                "Failed to construct 'ClipboardItem': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
            )),
            Native::FetchAbort(id) => self.abort_fetch(id),
            Native::Source(member) => self.call_source(member, this),
            Native::SourceReconnect(id) => self.reconnect_source(id),
            Native::SourceConstructor => Err(self.type_error(
                "Failed to construct 'EventSource': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
            )),
            Native::ReportError => {
                let error = self.throw_value(first);
                self.report_exception(error);
                Ok(Value::Undefined)
            }
            Native::DomExceptionConstructor => Err(self.type_error(
                "Failed to construct 'DOMException': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
            )),
            Native::RectConstructor(_) | Native::PointConstructor(_) => Err(self.type_error(
                "Failed to construct: Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
            )),
            Native::ObserverConstructor(kind) => Err(self.type_error(format!(
                "Failed to construct '{}': Please use the 'new' operator",
                kind.name()
            ))),
            Native::ParseInt => {
                let text = self.coerce_string(first)?;
                let radix = self.coerce_int32(argument(&arguments, 1))?;
                Ok(Value::Number(super::numbers::parse_int(&text, radix)))
            }
            Native::ParseFloat => {
                let text = self.coerce_string(first)?;
                Ok(Value::Number(super::numbers::parse_float(&text)))
            }
            // every call through the native is an indirect eval, so source runs as
            // global code with only its own directive deciding strictness
            Native::Eval => match first {
                Value::String(source) => self.evaluate_indirect(&source),
                other => Ok(other),
            },
            Native::EncodeUri(component) => {
                let text = self.coerce_string(first)?;
                let keep = if component {
                    ""
                } else {
                    super::strings::URI_RESERVED
                };
                Ok(Value::String(
                    super::strings::encode_uri(&text, keep).into(),
                ))
            }
            Native::CssEscape => {
                if arguments.is_empty() {
                    return Err(self.type_error("CSS.escape requires an argument"));
                }
                let text = self.coerce_string(first)?;
                Ok(Value::String(super::strings::css_escape(&text).into()))
            }
            // CSS.supports(property, value) or CSS.supports(condition), where a condition
            // that doesn't parse is tried again wrapped in parentheses
            Native::CssSupports => {
                if arguments.is_empty() {
                    return Err(self.type_error("CSS.supports requires an argument"));
                }
                let first_text = self.coerce_string(first)?;
                let supported = match arguments.get(1) {
                    Some(value) => {
                        let value = self.coerce_string(value.clone())?;
                        crate::media::supports(&format!("({first_text}: {value})"))
                    }
                    None => {
                        crate::media::supports(&first_text)
                            || crate::media::supports(&format!("({first_text})"))
                    }
                };
                Ok(Value::Bool(supported))
            }
            Native::DecodeUri(component) => {
                let text = self.coerce_string(first)?;
                let keep = if component {
                    ""
                } else {
                    super::strings::URI_RESERVED
                };
                match super::strings::decode_uri(&text, keep) {
                    Some(decoded) => Ok(Value::String(decoded.into())),
                    None => Err(self.throw_error("URIError", "URI malformed")),
                }
            }
            Native::GlobalIsNaN => Ok(Value::Bool(self.coerce_number(first)?.is_nan())),
            Native::GlobalIsFinite => Ok(Value::Bool(self.coerce_number(first)?.is_finite())),
            Native::StringRaw => {
                let strings = first;
                let raw = self.get_property(&strings, "raw")?;
                let parts = self.array_like_list(&raw)?;
                let mut text = String::new();
                for (index, part) in parts.iter().enumerate() {
                    text.push_str(&self.coerce_string(part.clone())?);
                    if index + 1 < parts.len()
                        && let Some(substitution) = arguments.get(index + 1)
                    {
                        text.push_str(&self.coerce_string(substitution.clone())?);
                    }
                }
                Ok(Value::String(text.into()))
            }
            Native::StringFromCodePoint => {
                let mut text = String::with_capacity(arguments.len());
                for value in arguments {
                    let number = self.coerce_number(value)?;
                    let character = (number.fract() == 0.0
                        && (0.0..=1_114_111.0).contains(&number))
                    .then(|| char::from_u32(number as u32).unwrap_or('\u{fffd}'));
                    match character {
                        Some(character) => text.push(character),
                        None => {
                            return Err(self.range_error(format!(
                                "Invalid code point {}",
                                number_to_string(number)
                            )));
                        }
                    }
                }
                Ok(Value::String(text.into()))
            }
            Native::Reflect(method) => self.call_reflect(method, arguments, None),
            Native::ProxyRevoke(proxy) => {
                *proxy.target.borrow_mut() = None;
                *proxy.handler.borrow_mut() = None;
                Ok(Value::Undefined)
            }
            Native::Collection(method) => self.call_collection(method, this, arguments, None),
            Native::Date(method) => self.call_date(method, this, arguments, None),
            Native::Typed(method) => self.call_typed(method, this, arguments, None),
            Native::Codec(method) => self.call_codec(method, this, arguments, None),
            Native::BigIntCall => {
                let primitive = self.coerce_primitive(first, super::operations::Hint::Number)?;
                match primitive {
                    Value::Number(number) => match super::bigint::BigInt::from_f64(number) {
                        Some(value) => Ok(Value::BigInt(Rc::new(value))),
                        None => Err(self.range_error(format!(
                            "The number {} cannot be converted to a BigInt because it is not an integer",
                            number_to_string(number)
                        ))),
                    },
                    other => Ok(Value::BigInt(self.coerce_bigint(other)?)),
                }
            }
            Native::BigIntAsN(signed) => {
                let bits = self.coerce_number(first)?;
                let bits = if bits.is_nan() { 0.0 } else { bits.trunc() };
                if !(0.0..=9_007_199_254_740_991.0).contains(&bits) {
                    return Err(
                        self.range_error("Invalid value: not (convertible to) a safe integer")
                    );
                }
                let value = self.coerce_bigint(argument(&arguments, 1))?;
                Ok(Value::BigInt(Rc::new(
                    value.as_bits(bits.min(1_048_576.0) as usize, signed),
                )))
            }
            Native::BigIntToString | Native::BigIntValueOf => {
                let value = match &this {
                    Value::BigInt(value) => Some(value.clone()),
                    Value::Object(object) => match &object.borrow().class {
                        ObjectClass::BigInt(value) => Some(value.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                let Some(value) = value else {
                    return Err(
                        self.type_error("BigInt.prototype method requires that 'this' be a BigInt")
                    );
                };
                if matches!(native, Native::BigIntValueOf) {
                    return Ok(Value::BigInt(value));
                }
                let radix = match first {
                    Value::Undefined => 10.0,
                    radix => self.coerce_number(radix)?.trunc(),
                };
                if !(2.0..=36.0).contains(&radix) {
                    return Err(self.range_error("toString() radix must be between 2 and 36"));
                }
                Ok(Value::String(value.to_string_radix(radix as u32).into()))
            }
            Native::Intl(method) => self.call_intl((*method).clone(), this, arguments),
            Native::NumericCompare => {
                if let (Value::BigInt(left), Value::BigInt(right)) =
                    (&first, &argument(&arguments, 1))
                {
                    return Ok(Value::Number(match left.compare(right) {
                        std::cmp::Ordering::Less => -1.0,
                        std::cmp::Ordering::Equal => 0.0,
                        std::cmp::Ordering::Greater => 1.0,
                    }));
                }
                let left = self.coerce_number(first)?;
                let right = self.coerce_number(argument(&arguments, 1))?;
                Ok(Value::Number(match (left.is_nan(), right.is_nan()) {
                    (true, true) => 0.0,
                    (true, false) => 1.0,
                    (false, true) => -1.0,
                    _ if left < right
                        || (left == 0.0
                            && right == 0.0
                            && left.is_sign_negative()
                            && !right.is_sign_negative()) =>
                    {
                        -1.0
                    }
                    _ if left > right
                        || (left == 0.0
                            && right == 0.0
                            && !left.is_sign_negative()
                            && right.is_sign_negative()) =>
                    {
                        1.0
                    }
                    _ => 0.0,
                }))
            }
            Native::JsonParse => self.json_parse(first, argument(&arguments, 1)),
            Native::JsonRawJson => self.raw_json(first),
            Native::JsonIsRawJson => Ok(Value::Bool(matches!(
                &first,
                Value::Object(object) if matches!(object.borrow().class, ObjectClass::RawJson)
            ))),
            Native::JsonStringify => {
                let text =
                    self.json_stringify(first, argument(&arguments, 1), argument(&arguments, 2))?;
                Ok(text.map_or(Value::Undefined, |text| Value::String(text.into())))
            }
        }
    }

    pub(super) fn construct_native(
        &mut self,
        native: Native,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let first = argument(&arguments, 0);
        let subclass = matches!(new_target, Value::Object(_));
        let prototype_for =
            |runtime: &mut Self, fallback: ObjectHandle| -> Result<ObjectHandle, ScriptError> {
                if subclass {
                    runtime.prototype_from_constructor(&new_target)
                } else {
                    Ok(fallback)
                }
            };
        match native {
            Native::ObjectConstructor => {
                if !matches!(first, Value::Undefined | Value::Null) && !subclass {
                    return self.coerce_object(first);
                }
                let fallback = self.intrinsics.object_prototype.clone();
                let prototype = prototype_for(self, fallback)?;
                Ok(Value::Object(new_object(JsObject::with_prototype(
                    Some(prototype),
                    ObjectClass::Ordinary,
                ))))
            }
            Native::FunctionConstructor => {
                let mut parameters = Vec::new();
                for value in arguments.iter().take(arguments.len().saturating_sub(1)) {
                    parameters.push(self.coerce_string(value.clone())?);
                }
                let body = match arguments.last() {
                    Some(body) => self.coerce_string(body.clone())?,
                    None => String::new(),
                };
                if let Err(message) = self.content_security_policy().eval() {
                    self.report(message.clone());
                    return Err(self.throw_error("EvalError", message));
                }
                self.compile_function_text(&parameters.join(","), &body)
            }
            Native::ErrorConstructor(kind) => {
                let target = if subclass {
                    new_target
                } else {
                    Value::Undefined
                };
                self.construct_error_object(kind, arguments, &target)
            }
            Native::BooleanConstructor | Native::NumberConstructor | Native::StringConstructor => {
                let primitive = self.call_native(native.clone(), Value::Undefined, arguments)?;
                let wrapped = self.coerce_object(primitive)?;
                if subclass && let Value::Object(object) = &wrapped {
                    let prototype = self.prototype_from_constructor(&new_target)?;
                    object.borrow_mut().set_prototype(Some(prototype));
                }
                Ok(wrapped)
            }
            Native::ArrayConstructor => {
                let values = if let [Value::Number(length)] = arguments.as_slice() {
                    let length = *length;
                    if length < 0.0 || length.fract() != 0.0 || length > 4_294_967_295.0 {
                        return Err(self.range_error("Invalid array length"));
                    }
                    if length > MAX_RUNTIME_ARRAY_LENGTH as f64 {
                        return Err(
                            self.range_error("array exceeds the deterministic length limit")
                        );
                    }
                    vec![Value::Uninitialized; length as usize]
                } else {
                    arguments
                };
                let array = Self::new_array(values);
                if subclass && let Value::Array(handle) = &array {
                    let prototype = self.prototype_from_constructor(&new_target)?;
                    handle.borrow_mut().prototype = Some(Some(prototype));
                }
                Ok(array)
            }
            Native::RegExp(method) => {
                self.call_regexp(method, Value::Undefined, arguments, Some(new_target))
            }
            Native::Reflect(method) => self.call_reflect(method, arguments, Some(new_target)),
            Native::Collection(method) => {
                self.call_collection(method, Value::Undefined, arguments, Some(new_target))
            }
            Native::Date(method) => {
                self.call_date(method, Value::Undefined, arguments, Some(new_target))
            }
            Native::Typed(method) => {
                let made = self.call_typed(
                    method,
                    Value::Undefined,
                    arguments,
                    Some(new_target.clone()),
                )?;
                // a subclass hands its own prototype to the typed array it constructs
                if let (Value::TypedArray(array), Value::Object(_)) = (&made, &new_target)
                    && let Value::Object(prototype) = self.get_property(&new_target, "prototype")?
                    && !self
                        .prototype_of(&made)
                        .is_some_and(|current| Rc::ptr_eq(&current, &prototype))
                {
                    super::typed_object(array)
                        .borrow_mut()
                        .set_prototype(Some(prototype));
                }
                Ok(made)
            }
            Native::DomConstructor(name) => self.construct_dom(name, arguments, new_target),
            Native::EventConstructor(name) => self.construct_event(name, arguments, new_target),
            Native::RangeConstructor(name) => self.construct_range(name, new_target),
            Native::ObserverConstructor(kind) => {
                self.construct_observer(kind, arguments, new_target)
            }
            Native::XhrConstructor => self.construct_xhr(new_target),
            Native::SocketConstructor => self.construct_socket(arguments, new_target),
            Native::SourceConstructor => self.construct_source(arguments, new_target),
            Native::NoticeConstructor => self.construct_notice(arguments, new_target),
            Native::ChannelConstructor => self.construct_channel(arguments, new_target),
            Native::ReaderConstructor => self.construct_reader(new_target),
            Native::MessageChannelConstructor => self.construct_message_channel(new_target),
            Native::AnimConstructor => Err(self.type_error("Illegal constructor")),
            Native::DomExceptionConstructor => {
                let message = match argument(&arguments, 0) {
                    Value::Undefined => String::new(),
                    value => self.coerce_string(value)?,
                };
                let name = match argument(&arguments, 1) {
                    Value::Undefined => "Error".to_string(),
                    value => self.coerce_string(value)?,
                };
                Ok(super::dom_exception(&name, &message))
            }
            Native::Helper(_) => self.construct_iterator(new_target),
            Native::Markup(_) => self.construct_markup(new_target),
            Native::Path(_) => self.construct_path(arguments, new_target),
            Native::Segmenter(_) => self.construct_segmenter(arguments, new_target),
            Native::Disposable(_, asynchronous) => {
                self.construct_disposable(asynchronous, new_target)
            }
            Native::Cssom(super::cssom::CssomMember::SheetConstructor) => {
                self.construct_sheet(arguments, new_target)
            }
            Native::Cssom(_) => Err(self.type_error("Illegal constructor")),
            Native::Font(super::fontfaces::FontMember::FaceConstructor) => {
                self.construct_font_face(arguments, new_target)
            }
            Native::Font(_) => Err(self.type_error("Illegal constructor")),
            Native::MatrixConstructor(read_only) => {
                self.construct_matrix(read_only, arguments, new_target)
            }
            Native::Stream(function) => self.construct_stream(&function, arguments, new_target),
            Native::Promise(function)
                if matches!(*function, super::promises::PromiseFunction::Constructor) =>
            {
                self.construct_promise_object(arguments, new_target)
            }
            Native::ClipboardItemConstructor => {
                self.call_clipboard_member(super::Builtin::ClipboardItemConstructor, arguments)
            }
            Native::RectConstructor(read_only) => {
                self.construct_rect(read_only, arguments, new_target)
            }
            Native::PointConstructor(read_only) => {
                self.construct_point(read_only, arguments, new_target)
            }
            Native::DataTransferConstructor => Ok(self.new_data_transfer()),
            Native::QuadConstructor => self.construct_quad(arguments, new_target),
            Native::Locale(_) => self.construct_locale(arguments, new_target),
            Native::DisplayNames(_) => self.construct_display_names(arguments, new_target),
            Native::DurationFormat(_) => self.construct_duration_format(arguments, new_target),
            Native::Offscreen(_) => self.construct_offscreen(arguments, new_target),
            Native::UrlPattern(_) => self.construct_url_pattern(arguments, new_target),
            Native::XPath(_) => self.construct_xpath_evaluator(new_target),
            Native::Temporal(super::temporal::TemporalMember::Construct(kind)) => {
                self.construct_temporal(kind, arguments, new_target)
            }
            Native::StyleOm(super::typed_om::StyleMember::Construct(name)) => {
                self.construct_style_om(name, arguments, new_target)
            }
            Native::PerfObserver(member) => {
                self.construct_perf_observer(member, arguments, new_target)
            }
            Native::PortConstructor | Native::IllegalConstructor => {
                Err(self.type_error("Illegal constructor"))
            }
            Native::UrlConstructor => self.construct_url(arguments, new_target),
            Native::UrlSearchParamsConstructor => {
                self.construct_url_search_params(arguments, new_target)
            }
            Native::AudioConstructor(class) => self.construct_audio(class, arguments, new_target),
            Native::Codec(method) => {
                self.call_codec(method, Value::Undefined, arguments, Some(new_target))
            }
            Native::Intl(method) => self.call_intl((*method).clone(), Value::Undefined, arguments),
            other => {
                let name = format!("{other:?}");
                Err(self.type_error(format!("{name} is not a constructor")))
            }
        }
    }

    // ValidateAndApplyPropertyDescriptor for an array: an element or name keeps its plain
    // slot while it is an ordinary writable, enumerable, configurable value, and moves to the
    // array's property list once an accessor or other attributes set it apart
    fn define_array_property(
        &mut self,
        array: &ArrayHandle,
        key: String,
        mut property: Property,
        present: [bool; 6],
    ) -> Result<(), ScriptError> {
        if key == "length" {
            // length is a data property that is never enumerable or configurable
            let accessor = present[4] || present[5];
            if accessor
                || (present[1] && property.configurable)
                || (present[0] && property.enumerable)
            {
                return Err(self.type_error("Cannot redefine property: length"));
            }
            let fixed =
                array.borrow().fixed_length || array.borrow().integrity == super::Integrity::Frozen;
            if fixed && present[3] && property.writable {
                return Err(self.type_error("Cannot redefine property: length"));
            }
            if let Slot::Data(value) = &property.slot
                && present[2]
            {
                set_array_length(array, value.clone())?;
            }
            if present[3] && !property.writable {
                array.borrow_mut().fixed_length = true;
            }
            return Ok(());
        }
        let index = array_index(&key).map(|index| index as usize);
        let current = self.own_property_of(&Value::Array(array.clone()), &key)?;
        let refuse =
            |runtime: &mut Self| runtime.type_error(format!("Cannot redefine property: {key}"));
        match &current {
            None => {
                if array.borrow().integrity != super::Integrity::Open {
                    return Err(self.type_error(format!(
                        "Cannot define property {key}, object is not extensible"
                    )));
                }
            }
            Some(current) => {
                let accessor = present[4] || present[5];
                let data = present[2] || present[3];
                let is_accessor = matches!(current.slot, Slot::Accessor { .. });
                if !current.configurable {
                    let changes_kind = (accessor && !is_accessor) || (data && is_accessor);
                    let unchanged = match (&current.slot, &property.slot) {
                        (Slot::Data(old), Slot::Data(new)) => {
                            super::operations::same_value(old, new)
                        }
                        _ => true,
                    };
                    let same_accessors = match (&current.slot, &property.slot) {
                        (
                            Slot::Accessor { get, set },
                            Slot::Accessor {
                                get: new_get,
                                set: new_set,
                            },
                        ) => {
                            (!present[4]
                                || super::operations::same_value(
                                    get.as_ref().unwrap_or(&Value::Undefined),
                                    new_get.as_ref().unwrap_or(&Value::Undefined),
                                ))
                                && (!present[5]
                                    || super::operations::same_value(
                                        set.as_ref().unwrap_or(&Value::Undefined),
                                        new_set.as_ref().unwrap_or(&Value::Undefined),
                                    ))
                        }
                        _ => true,
                    };
                    let invalid = (present[1] && property.configurable)
                        || (present[0] && property.enumerable != current.enumerable)
                        || changes_kind
                        || (!is_accessor
                            && !current.writable
                            && ((present[3] && property.writable) || (present[2] && !unchanged)))
                        || (is_accessor && !same_accessors);
                    if invalid {
                        return Err(refuse(self));
                    }
                }
                // what the descriptor leaves out keeps its current setting
                if !present[0] {
                    property.enumerable = current.enumerable;
                }
                if !present[1] {
                    property.configurable = current.configurable;
                }
                match (&current.slot, &mut property.slot) {
                    (Slot::Data(old), Slot::Data(new)) if !accessor => {
                        if !present[2] {
                            *new = old.clone();
                        }
                        if !present[3] {
                            property.writable = current.writable;
                        }
                    }
                    (
                        Slot::Accessor { get, set },
                        Slot::Accessor {
                            get: new_get,
                            set: new_set,
                        },
                    ) => {
                        if !present[4] {
                            *new_get = get.clone();
                        }
                        if !present[5] {
                            *new_set = set.clone();
                        }
                    }
                    _ => {}
                }
            }
        }
        let plain = matches!(property.slot, Slot::Data(_))
            && property.writable
            && property.enumerable
            && property.configurable;
        let mut data = array.borrow_mut();
        data.properties.retain(|(name, _)| *name != key);
        match (index, plain) {
            (Some(index), true) => {
                let Slot::Data(value) = property.slot else {
                    unreachable!("plain is data")
                };
                drop(data);
                set_array_element_unchecked(array, index, value)
            }
            (Some(index), false) => {
                if index >= MAX_RUNTIME_ARRAY_LENGTH {
                    return Err(
                        self.range_error("array index exceeds the deterministic length limit")
                    );
                }
                if index >= data.len() {
                    data.resize(index + 1, Value::Uninitialized);
                }
                data[index] = Value::Uninitialized;
                data.properties.push((key, property));
                Ok(())
            }
            (None, _) => {
                data.properties.push((key, property));
                Ok(())
            }
        }
    }

    // an array's own prototype; one closed to extension keeps the one it has
    pub(super) fn set_array_prototype(
        &mut self,
        array: &ArrayHandle,
        prototype: Option<ObjectHandle>,
    ) -> bool {
        let current = self.prototype_of(&Value::Array(array.clone()));
        let same = match (&current, &prototype) {
            (Some(current), Some(prototype)) => Rc::ptr_eq(current, prototype),
            (None, None) => true,
            _ => false,
        };
        if same {
            return true;
        }
        if array.borrow().integrity != super::Integrity::Open {
            return false;
        }
        array.borrow_mut().prototype = Some(prototype);
        true
    }

    pub(super) fn set_object_prototype(
        &mut self,
        object: &ObjectHandle,
        prototype: Option<ObjectHandle>,
    ) -> Result<(), ScriptError> {
        if let Some(proxy) = super::reflect::proxy_of(object) {
            let (target, trap) = self.proxy_trap(&proxy, "setPrototypeOf")?;
            let value = prototype.clone().map(Value::Object).unwrap_or(Value::Null);
            return match trap {
                Some(trap) => {
                    let handler = proxy.handler.borrow().clone().unwrap_or(Value::Undefined);
                    if self
                        .call_function(trap, handler, vec![target, value])?
                        .truthy()
                    {
                        Ok(())
                    } else {
                        Err(self.type_error("'setPrototypeOf' on proxy: trap returned falsish"))
                    }
                }
                None => match &target {
                    Value::Object(target) => self.set_object_prototype(target, prototype),
                    _ => Ok(()),
                },
            };
        }
        let unchanged = match (&object.borrow().prototype_or(&self.intrinsics), &prototype) {
            (Some(current), Some(next)) => Rc::ptr_eq(current, next),
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return Ok(());
        }
        if !object.borrow().extensible {
            return Err(self.type_error("#<Object> is not extensible"));
        }
        let mut current = prototype.clone();
        while let Some(link) = current {
            if Rc::ptr_eq(&link, object) {
                return Err(self.type_error("Cyclic __proto__ value"));
            }
            current = link.borrow().prototype_or(&self.intrinsics);
        }
        object.borrow_mut().set_prototype(prototype);
        Ok(())
    }

    pub(super) fn object_to_string(&mut self, this: &Value) -> Result<String, ScriptError> {
        let builtin = match this {
            Value::Undefined => return Ok("[object Undefined]".to_string()),
            Value::Null => return Ok("[object Null]".to_string()),
            array if self.is_array(array)? => "Array",
            Value::Bool(_) => "Boolean",
            Value::Number(_) => "Number",
            Value::String(_) => "String",
            Value::Error(_) => "Error",
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Function(_) | ObjectClass::Bound { .. } | ObjectClass::Native(_) => {
                    "Function"
                }
                ObjectClass::Error => "Error",
                ObjectClass::Boolean(_) => "Boolean",
                ObjectClass::Number(_) => "Number",
                ObjectClass::String(_) => "String",
                ObjectClass::Arguments => "Arguments",
                _ => "Object",
            },
            other if is_callable(other) => "Function",
            other => {
                let text = other.string_value();
                if let Some(tag) = text
                    .strip_prefix("[object ")
                    .and_then(|rest| rest.strip_suffix(']'))
                {
                    return Ok(format!("[object {tag}]"));
                }
                "Object"
            }
        };
        // primitives read the tag through their prototype, as ToObject would
        let tag = self.get_property(this, "@@toStringTag")?;
        Ok(match tag {
            Value::String(tag) => format!("[object {tag}]"),
            _ => format!("[object {builtin}]"),
        })
    }
}

fn same_optional(left: &Option<Value>, right: &Option<Value>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => same_value(left, right),
        (None, None) => true,
        _ => false,
    }
}

fn to_uint32_from(value: f64) -> u32 {
    super::operations::to_uint32_number(value)
}

fn string_own_property(text: &str, key: &str) -> Option<Property> {
    if key == "length" {
        return Some(fixed(Value::Number(
            super::strings::utf16_length(text) as f64
        )));
    }
    let index = array_index(key)?;
    super::strings::unit_at(text, index as usize).map(|unit| Property {
        slot: Slot::Data(unit),
        enumerable: true,
        writable: false,
        configurable: false,
    })
}

// Number.prototype.toString for radixes other than ten
fn number_to_radix(value: f64, radix: u32) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if value == 0.0 {
        return "0".to_string();
    }
    let negative = value < 0.0;
    let value = value.abs();
    let mut integer = value.trunc();
    let mut fraction = value - integer;
    let mut digits = Vec::new();
    while integer >= 1.0 {
        let digit = (integer % f64::from(radix)) as u32;
        digits.push(std::char::from_digit(digit, radix).expect("digit in radix"));
        integer = (integer / f64::from(radix)).trunc();
    }
    if digits.is_empty() {
        digits.push('0');
    }
    digits.reverse();
    let mut text: String = digits.into_iter().collect();
    if fraction > 0.0 {
        text.push('.');
        // enough digits to distinguish the value at double precision
        let mut emitted = 0;
        let precision = (52.0 / f64::from(radix).log2()).ceil() as usize + 1;
        while fraction > 0.0 && emitted < precision {
            fraction *= f64::from(radix);
            let digit = fraction.trunc() as u32;
            text.push(std::char::from_digit(digit, radix).expect("digit in radix"));
            fraction -= f64::from(digit);
            emitted += 1;
        }
    }
    if negative {
        text.insert(0, '-');
    }
    text
}

// a string as a pattern that matches it exactly: syntax characters take a backslash, and a
// leading letter or digit, punctuators and white space are written as escapes, so the result
// can follow anything in a larger pattern
pub(super) fn regexp_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() * 2);
    for (index, character) in text.chars().enumerate() {
        let code = u32::from(character);
        let hex = || {
            if code <= 0xff {
                format!("\\x{code:02x}")
            } else {
                format!("\\u{code:04x}")
            }
        };
        let piece = match character {
            _ if index == 0 && character.is_ascii_alphanumeric() => hex(),
            _ if "^$\\.*+?()[]{}|/".contains(character) => format!("\\{character}"),
            '\t' => "\\t".to_string(),
            '\n' => "\\n".to_string(),
            '\u{b}' => "\\v".to_string(),
            '\u{c}' => "\\f".to_string(),
            '\r' => "\\r".to_string(),
            _ if ",-=<>#&!%:;@~'`\"".contains(character)
                || character.is_whitespace()
                || character == '\u{feff}' =>
            {
                hex()
            }
            _ => character.to_string(),
        };
        escaped.push_str(&piece);
    }
    escaped
}
