// the intrinsic objects every script sees (Object, Function, Symbol, the Error family,
// Boolean, Number, String, Array and the iterator and generator prototypes) and the native
// functions behind their methods

use super::ast::FunctionKind;
use super::compiler::Compiler;
use super::object::{
    ArrayIteration, ArrayIterationKind, JsObject, ObjectClass, ObjectHandle, Property,
    SYMBOL_PREFIX, Slot, array_index, is_private_key, is_symbol_key, new_object,
};
use super::operations::{
    is_callable, is_primitive, number_to_string, same_value, symbol_description,
};
use super::vm::{AsyncResumeHandle, ResumeMode};
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
    // true for the component forms
    EncodeUri(bool),
    DecodeUri(bool),
    GlobalIsNaN,
    GlobalIsFinite,
    StringRaw,
    StringFromCodePoint,
    JsonParse,
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
    Notice(super::notifications::NoticeMember),
    NoticeConstructor,
    Url(super::urls::UrlMember),
    UrlConstructor,
    UrlSearchParams(super::urls::ParamsMember),
    UrlSearchParamsConstructor,
    // an asynchronous XMLHttpRequest reporting its answer
    XhrDeliver(u32, u64),
    // an img reporting the outcome of the request it started in this generation
    ImageDeliver(crate::dom::NodeId, u64),
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
                | Native::NoticeConstructor
                | Native::UrlConstructor
                | Native::UrlSearchParamsConstructor
                | Native::AudioConstructor(_)
        ) || matches!(self, Native::Intl(method) if matches!(**method,
            super::intl::IntlMethod::NumberFormatConstructor
                | super::intl::IntlMethod::DateTimeFormatConstructor
                | super::intl::IntlMethod::CollatorConstructor
                | super::intl::IntlMethod::PluralRulesConstructor
                | super::intl::IntlMethod::RelativeTimeFormatConstructor))
    }
}

impl fmt::Debug for Native {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Native")
    }
}

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
    pub(super) promise_prototype: ObjectHandle,
    pub(super) regexp_prototype: ObjectHandle,
    pub(super) array_values: Value,
    pub(super) iterator_prototype: ObjectHandle,
    pub(super) array_iterator_prototype: ObjectHandle,
    pub(super) map_prototype: ObjectHandle,
    pub(super) set_prototype: ObjectHandle,
    pub(super) weak_map_prototype: ObjectHandle,
    pub(super) weak_set_prototype: ObjectHandle,
    pub(super) weak_ref_prototype: ObjectHandle,
    pub(super) collection_iterator_prototype: ObjectHandle,
    pub(super) date_prototype: ObjectHandle,
    pub(super) typed_prototypes: Vec<ObjectHandle>,
    pub(super) array_buffer_prototype: ObjectHandle,
    pub(super) data_view_prototype: ObjectHandle,
    pub(super) text_encoder_prototype: ObjectHandle,
    pub(super) bigint_prototype: ObjectHandle,
    pub(super) text_decoder_prototype: ObjectHandle,
    // NumberFormat, DateTimeFormat, Collator, PluralRules and RelativeTimeFormat
    pub(super) intl_prototypes: Vec<ObjectHandle>,
    pub(super) dom: super::interfaces::DomInterfaces,
    pub(super) events: super::events::EventInterfaces,
    pub(super) ranges: super::ranges::RangeInterfaces,
    pub(super) observers: super::observers::ObserverInterfaces,
    pub(super) xhr: super::xhr::XhrInterfaces,
    pub(super) sockets: super::sockets::SocketInterfaces,
    pub(super) notices: super::notifications::NoticeInterfaces,
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

    fn value(&self, target: &ObjectHandle, key: &str, value: Value) {
        target
            .borrow_mut()
            .define(key.to_string(), Property::hidden(value));
    }

    fn getter(&self, target: &ObjectHandle, key: &str, getter: Native, setter: Option<Native>) {
        let get = Value::Object(self.function(getter, &format!("get {key}"), 0));
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
    ] {
        let prototype = plain(&error_prototype);
        build.value(&prototype, "name", Value::String(kind.to_string().into()));
        build.value(&prototype, "message", Value::String(String::new().into()));
        let length = if kind == "AggregateError" { 2 } else { 1 };
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
    ]: [ObjectHandle; 7] = error_prototypes
        .try_into()
        .unwrap_or_else(|_| unreachable!("seven error kinds"));

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
    globals.push(("parseFloat", parse_float));
    for (name, native) in [
        ("encodeURI", Native::EncodeUri(false)),
        ("encodeURIComponent", Native::EncodeUri(true)),
        ("decodeURI", Native::DecodeUri(false)),
        ("decodeURIComponent", Native::DecodeUri(true)),
    ] {
        globals.push((name, Value::Object(build.function(native, name, 1))));
    }
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
    build.value(&json, "@@toStringTag", Value::String("JSON".into()));
    globals.push(("JSON", Value::Object(json)));

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
    globals.push(("NaN", Value::Number(f64::NAN)));
    globals.push(("Infinity", Value::Number(f64::INFINITY)));

    let dom = super::interfaces::create(&build, &object_prototype, &mut globals);
    let events = super::events::create(&build, &object_prototype, &mut globals);
    let ranges = super::ranges::create(&build, &object_prototype, &mut globals);
    let observers = super::observers::create(&build, &object_prototype, &mut globals);
    let xhr = super::xhr::create(&build, &dom.prototype("EventTarget"), &mut globals);
    let sockets = super::sockets::create(&build, &dom.prototype("EventTarget"), &mut globals);
    let notices = super::notifications::create(&build, &dom.prototype("EventTarget"), &mut globals);
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
        promise_prototype,
        regexp_prototype,
        array_values,
        iterator_prototype,
        array_iterator_prototype,
        map_prototype,
        set_prototype,
        weak_map_prototype,
        weak_set_prototype,
        weak_ref_prototype,
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
        notices,
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
            Value::Array(array) => {
                if key == "length" {
                    Some(Property {
                        slot: Slot::Data(Value::Number(array.borrow().len() as f64)),
                        enumerable: false,
                        writable: true,
                        configurable: false,
                    })
                } else if let Some(index) = array_index(key) {
                    array
                        .borrow()
                        .get(index as usize)
                        .cloned()
                        .map(Property::data)
                } else {
                    self.array_expando(array, key).map(Property::data)
                }
            }
            Value::Window => {
                let global = self.global.borrow();
                global.accessors.get(key).cloned().or_else(|| {
                    global
                        .values
                        .get(key)
                        .map(|value| Property::data(value.resolve()))
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
        Ok(self.own_keys(value, symbols))
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
            Value::Array(array) if !symbols => {
                let mut keys: Vec<String> = (0..array.borrow().len())
                    .map(|index| index.to_string())
                    .collect();
                keys.push("length".to_string());
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
            Value::Array(array) => {
                if let Slot::Data(value) = property.slot {
                    if key == "length" {
                        return set_array_length(array, value);
                    }
                    if let Some(index) = array_index(&key) {
                        return set_array_element(array, index as usize, value);
                    }
                    self.set_array_expando(array, &key, value);
                    return Ok(());
                }
                return Err(self.type_error("accessor properties on arrays are not supported"));
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
                    Slot::Data(value) => {
                        global.accessors.remove(&key);
                        global.values.insert(key, value);
                    }
                    Slot::Accessor { .. } => {
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
            Value::Array(array) => Ok(array.borrow().clone()),
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
                if let Value::Object(object) = &first {
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
                    value => is_primitive(value),
                }))
            }
            Native::ObjectIsExtensible => Ok(Value::Bool(match &first {
                Value::Object(object) => object.borrow().extensible,
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
            Native::ErrorCaptureStackTrace => {
                if let Value::Object(object) = &first {
                    let text = self.coerce_string(first.clone())?;
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
            Native::Array(method) => self.call_array_method(method, this, arguments),
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
                    vec![Value::Undefined; length as usize]
                } else {
                    arguments
                };
                Ok(Self::new_array(values))
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
                self.call_typed(method, Value::Undefined, arguments, Some(new_target))
            }
            Native::DomConstructor(name) => self.construct_dom(name, arguments, new_target),
            Native::EventConstructor(name) => self.construct_event(name, arguments, new_target),
            Native::RangeConstructor(name) => self.construct_range(name, new_target),
            Native::ObserverConstructor(kind) => {
                self.construct_observer(kind, arguments, new_target)
            }
            Native::XhrConstructor => self.construct_xhr(new_target),
            Native::SocketConstructor => self.construct_socket(arguments, new_target),
            Native::NoticeConstructor => self.construct_notice(arguments, new_target),
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
