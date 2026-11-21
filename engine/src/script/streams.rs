// ReadableStream from the streams standard: a queue of chunks fed by an underlying source
// through its controller and drained by a default reader, with cancel, tee, async iteration
// and ReadableStream.from; response bodies are streams of this kind fed from their bytes

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, new_object};
use super::operations::{is_callable, is_primitive, plain_object};
use super::realm::{Builder, Native};
use super::*;
use std::collections::VecDeque;

// the bytes a body hands out per read
const BODY_CHUNK: usize = 64 * 1024;

#[derive(Clone)]
pub(super) enum StreamHandle {
    Readable(Rc<RefCell<Readable>>),
    Controller(Rc<RefCell<Readable>>),
    Reader(Rc<RefCell<Reader>>),
    Iterator(Rc<RefCell<StreamIteration>>),
    Writable(Rc<RefCell<Writable>>),
    WritableController(Rc<RefCell<Writable>>),
    Writer(Rc<RefCell<Writer>>),
    TransformController(Rc<TransformState>),
    Pair(Rc<Pair>),
    // a CountQueuingStrategy or ByteLengthQueuingStrategy with its high-water mark
    Strategy(f64),
}

#[derive(Clone)]
pub(super) enum StreamFunction {
    Constructor,
    From,
    Locked,
    Cancel,
    GetReader,
    Tee,
    Values,
    DesiredSize,
    Enqueue,
    Close,
    Error,
    ReaderConstructor,
    // controllers are made only by the streams they belong to
    ControllerConstructor,
    Read,
    ReleaseLock,
    ReaderCancel,
    Closed,
    IteratorNext,
    IteratorReturn,
    // the source's start promise settled
    Started(Rc<RefCell<Readable>>, bool),
    // a pull promise settled
    Pulled(Rc<RefCell<Readable>>, bool),
    // a cancel promise settled: what was canceled has nothing to hand on
    Canceled,
    // an iterator over a source given to ReadableStream.from answered a step
    FromStep(Rc<RefCell<Readable>>, bool),
    // a tee's read of the original stream answered
    TeeRead(Rc<TeeState>, bool),
    // an async iterator's read settled; done readers let go of the stream
    IterationRead(Rc<RefCell<StreamIteration>>),
    // a value from a sync iterator given to ReadableStream.from settled
    FromValue(Rc<RefCell<Readable>>, bool),
    // a canceled iteration answers with what return was given
    ReturnDone(Value),
    WritableConstructor,
    WriterConstructor,
    WritableControllerConstructor,
    TransformConstructor,
    TransformControllerConstructor,
    EncoderStreamConstructor,
    DecoderStreamConstructor,
    // a DecompressionStream when true
    CompressionConstructor(bool),
    // a ByteLengthQueuingStrategy when true
    StrategyConstructor(bool),
    WritableLocked,
    WritableAbort,
    WritableClose,
    GetWriter,
    WritableError,
    WritableSignal,
    WriterWrite,
    WriterClose,
    WriterAbort,
    WriterReady,
    WriterClosed,
    WriterDesiredSize,
    WriterRelease,
    PipeTo,
    PipeThrough,
    PairReadable,
    PairWritable,
    PairEncoding,
    PairFatal,
    PairIgnoreBom,
    TransformDesiredSize,
    TransformEnqueue,
    TransformError,
    TransformTerminate,
    StrategyMark,
    // the size getters of the two strategies and the functions they hand out
    StrategySize,
    ByteSize,
    CountOf,
    ByteLengthOf,
    WriterStarted(Rc<RefCell<Writable>>, bool),
    // a sink's write, or its close when true, settled
    Wrote(Rc<RefCell<Writable>>, bool, bool),
    TransformAfterBackpressure(Rc<TransformState>, Value),
    TransformFailed(Rc<TransformState>),
    Flushed(Rc<TransformState>, bool),
    PipeReady(Rc<Pipe>, bool),
    PipeRead(Rc<Pipe>, bool),
    PipeClosed(Rc<Pipe>, bool),
    PipeSignal(Rc<Pipe>, AbortSignalHandle),
    // a body read to the end for text, json, arrayBuffer or blob answered a read
    Collected(Rc<Collect>, bool),
}

#[derive(Clone, Copy)]
pub(super) enum BodyKind {
    Text,
    Json,
    Bytes,
    Blob,
}

// a stream body being read to its end for one of the body methods
pub(super) struct Collect {
    reader: Rc<RefCell<Reader>>,
    bytes: RefCell<Vec<u8>>,
    kind: BodyKind,
    mime_type: String,
    result: PromiseHandle,
}

impl StreamFunction {
    pub(super) fn is_constructor(&self) -> bool {
        matches!(
            self,
            StreamFunction::Constructor
                | StreamFunction::ReaderConstructor
                | StreamFunction::ControllerConstructor
                | StreamFunction::WritableConstructor
                | StreamFunction::WriterConstructor
                | StreamFunction::WritableControllerConstructor
                | StreamFunction::TransformConstructor
                | StreamFunction::TransformControllerConstructor
                | StreamFunction::EncoderStreamConstructor
                | StreamFunction::DecoderStreamConstructor
                | StreamFunction::CompressionConstructor(_)
                | StreamFunction::StrategyConstructor(_)
        )
    }
}

#[derive(Clone)]
enum State {
    Readable,
    Closed,
    Errored(Value),
}

enum Source {
    // an underlying source object with the methods it has
    Script {
        object: Value,
        pull: Option<Value>,
        cancel: Option<Value>,
    },
    // the bytes of a response or request body
    Body(ResponseBodyHandle),
    // an async or sync iterator ReadableStream.from reads
    Iterator {
        iterator: Value,
        next: Value,
    },
    // one of the two branches of a tee
    Tee(Rc<TeeState>, usize),
    // the readable side of a transform
    Transform(Rc<TransformState>),
    // a body arriving from the network, which the fetch pump feeds; the flag says it was canceled
    Network(Rc<Cell<bool>>),
}

pub(super) struct Readable {
    state: State,
    queue: VecDeque<(Value, f64)>,
    queued: f64,
    high_water_mark: f64,
    size: Option<Value>,
    source: Source,
    started: bool,
    pulling: bool,
    pull_again: bool,
    close_requested: bool,
    disturbed: bool,
    reader: Option<Rc<RefCell<Reader>>>,
    controller: Option<ObjectHandle>,
}

pub(super) struct Reader {
    stream: Option<Rc<RefCell<Readable>>>,
    requests: VecDeque<PromiseHandle>,
    closed: PromiseHandle,
}

pub(super) struct TeeState {
    reader: Rc<RefCell<Reader>>,
    original: Rc<RefCell<Readable>>,
    branches: RefCell<Vec<Rc<RefCell<Readable>>>>,
    reading: Cell<bool>,
    read_again: Cell<bool>,
    canceled: [Cell<bool>; 2],
    reasons: RefCell<[Value; 2]>,
    cancel_promise: PromiseHandle,
}

pub(super) struct StreamIteration {
    reader: Rc<RefCell<Reader>>,
    prevent_cancel: bool,
    done: bool,
}

pub(super) struct StreamInterfaces {
    readable: ObjectHandle,
    controller: ObjectHandle,
    reader: ObjectHandle,
    iterator: ObjectHandle,
    writable: ObjectHandle,
    writable_controller: ObjectHandle,
    writer: ObjectHandle,
    transform: ObjectHandle,
    transform_controller: ObjectHandle,
    encoder: ObjectHandle,
    decoder: ObjectHandle,
    compression: ObjectHandle,
    count_strategy: ObjectHandle,
    byte_strategy: ObjectHandle,
}

use std::cell::Cell;

fn native(function: StreamFunction) -> Native {
    Native::Stream(Rc::new(function))
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    async_iterator_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> StreamInterfaces {
    use StreamFunction as F;
    let plain = |prototype: &ObjectHandle| {
        new_object(JsObject::with_prototype(
            Some(prototype.clone()),
            ObjectClass::Ordinary,
        ))
    };
    let method = |target: &ObjectHandle, name: &str, function: F, length: u32| {
        let made = build.function(native(function), name, length);
        target
            .borrow_mut()
            .define(name.to_string(), Property::hidden(Value::Object(made)));
    };
    let getter = |target: &ObjectHandle, name: &str, function: F| {
        let made = build.function(native(function), &format!("get {name}"), 0);
        target.borrow_mut().define(
            name.to_string(),
            Property {
                slot: object::Slot::Accessor {
                    get: Some(Value::Object(made)),
                    set: None,
                },
                enumerable: true,
                writable: false,
                configurable: true,
            },
        );
    };
    let tag = |target: &ObjectHandle, name: &str| {
        target.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property {
                writable: false,
                ..Property::hidden(Value::String(name.into()))
            },
        );
    };

    let readable = plain(object_prototype);
    getter(&readable, "locked", F::Locked);
    method(&readable, "cancel", F::Cancel, 0);
    method(&readable, "getReader", F::GetReader, 0);
    method(&readable, "tee", F::Tee, 0);
    method(&readable, "pipeTo", F::PipeTo, 1);
    method(&readable, "pipeThrough", F::PipeThrough, 1);
    let values = build.function(native(F::Values), "values", 0);
    for key in ["values", "@@asyncIterator"] {
        readable.borrow_mut().define(
            key.to_string(),
            Property::hidden(Value::Object(values.clone())),
        );
    }
    tag(&readable, "ReadableStream");
    let constructor = build.constructor(native(F::Constructor), "ReadableStream", 0, &readable);
    method(&constructor, "from", F::From, 1);
    globals.push(("ReadableStream", Value::Object(constructor)));

    let controller = plain(object_prototype);
    getter(&controller, "desiredSize", F::DesiredSize);
    method(&controller, "enqueue", F::Enqueue, 0);
    method(&controller, "close", F::Close, 0);
    method(&controller, "error", F::Error, 0);
    tag(&controller, "ReadableStreamDefaultController");
    let controller_constructor = build.constructor(
        native(F::ControllerConstructor),
        "ReadableStreamDefaultController",
        0,
        &controller,
    );
    globals.push((
        "ReadableStreamDefaultController",
        Value::Object(controller_constructor),
    ));

    let reader = plain(object_prototype);
    getter(&reader, "closed", F::Closed);
    method(&reader, "read", F::Read, 0);
    method(&reader, "releaseLock", F::ReleaseLock, 0);
    method(&reader, "cancel", F::ReaderCancel, 0);
    tag(&reader, "ReadableStreamDefaultReader");
    let reader_constructor = build.constructor(
        native(F::ReaderConstructor),
        "ReadableStreamDefaultReader",
        1,
        &reader,
    );
    globals.push((
        "ReadableStreamDefaultReader",
        Value::Object(reader_constructor),
    ));

    let iterator = plain(async_iterator_prototype);
    method(&iterator, "next", F::IteratorNext, 0);
    method(&iterator, "return", F::IteratorReturn, 1);
    tag(&iterator, "ReadableStream AsyncIterator");

    let class = |globals: &mut Vec<(&'static str, Value)>,
                 name: &'static str,
                 function: F,
                 length: u32,
                 prototype: &ObjectHandle| {
        tag(prototype, name);
        let made = build.constructor(native(function), name, length, prototype);
        globals.push((name, Value::Object(made.clone())));
        made
    };

    let writable = plain(object_prototype);
    getter(&writable, "locked", F::WritableLocked);
    method(&writable, "abort", F::WritableAbort, 0);
    method(&writable, "close", F::WritableClose, 0);
    method(&writable, "getWriter", F::GetWriter, 0);
    class(
        globals,
        "WritableStream",
        F::WritableConstructor,
        0,
        &writable,
    );

    let writable_controller = plain(object_prototype);
    getter(&writable_controller, "signal", F::WritableSignal);
    method(&writable_controller, "error", F::WritableError, 0);
    class(
        globals,
        "WritableStreamDefaultController",
        F::WritableControllerConstructor,
        0,
        &writable_controller,
    );

    let writer = plain(object_prototype);
    getter(&writer, "closed", F::WriterClosed);
    getter(&writer, "ready", F::WriterReady);
    getter(&writer, "desiredSize", F::WriterDesiredSize);
    method(&writer, "write", F::WriterWrite, 0);
    method(&writer, "close", F::WriterClose, 0);
    method(&writer, "abort", F::WriterAbort, 0);
    method(&writer, "releaseLock", F::WriterRelease, 0);
    class(
        globals,
        "WritableStreamDefaultWriter",
        F::WriterConstructor,
        1,
        &writer,
    );

    let pair = |getters: &[(&str, F)]| {
        let prototype = plain(object_prototype);
        getter(&prototype, "readable", F::PairReadable);
        getter(&prototype, "writable", F::PairWritable);
        for (name, function) in getters {
            getter(&prototype, name, function.clone());
        }
        prototype
    };
    let transform = pair(&[]);
    class(
        globals,
        "TransformStream",
        F::TransformConstructor,
        0,
        &transform,
    );
    let transform_controller = plain(object_prototype);
    getter(
        &transform_controller,
        "desiredSize",
        F::TransformDesiredSize,
    );
    method(&transform_controller, "enqueue", F::TransformEnqueue, 0);
    method(&transform_controller, "error", F::TransformError, 0);
    method(&transform_controller, "terminate", F::TransformTerminate, 0);
    class(
        globals,
        "TransformStreamDefaultController",
        F::TransformControllerConstructor,
        0,
        &transform_controller,
    );
    let encoder = pair(&[("encoding", F::PairEncoding)]);
    class(
        globals,
        "TextEncoderStream",
        F::EncoderStreamConstructor,
        0,
        &encoder,
    );
    let decoder = pair(&[
        ("encoding", F::PairEncoding),
        ("fatal", F::PairFatal),
        ("ignoreBOM", F::PairIgnoreBom),
    ]);
    class(
        globals,
        "TextDecoderStream",
        F::DecoderStreamConstructor,
        0,
        &decoder,
    );
    let compression = pair(&[]);
    class(
        globals,
        "CompressionStream",
        F::CompressionConstructor(false),
        1,
        &compression,
    );
    let decompression = pair(&[]);
    class(
        globals,
        "DecompressionStream",
        F::CompressionConstructor(true),
        1,
        &decompression,
    );

    let strategy =
        |name: &'static str, bytes: bool, size: F, globals: &mut Vec<(&'static str, Value)>| {
            let prototype = plain(object_prototype);
            getter(&prototype, "highWaterMark", F::StrategyMark);
            getter(&prototype, "size", size);
            class(globals, name, F::StrategyConstructor(bytes), 1, &prototype);
            prototype
        };
    let count_strategy = strategy("CountQueuingStrategy", false, F::StrategySize, globals);
    let byte_strategy = strategy("ByteLengthQueuingStrategy", true, F::ByteSize, globals);

    StreamInterfaces {
        readable,
        controller,
        reader,
        iterator,
        writable,
        writable_controller,
        writer,
        transform,
        transform_controller,
        encoder,
        decoder,
        compression,
        count_strategy,
        byte_strategy,
    }
}

fn readable_state(source: Source, high_water_mark: f64, size: Option<Value>) -> Readable {
    Readable {
        state: State::Readable,
        queue: VecDeque::new(),
        queued: 0.0,
        high_water_mark,
        size,
        source,
        started: false,
        pulling: false,
        pull_again: false,
        close_requested: false,
        disturbed: false,
        reader: None,
        controller: None,
    }
}

impl ScriptRuntime {
    fn stream_object(&self, prototype: &ObjectHandle, handle: StreamHandle) -> ObjectHandle {
        new_object(JsObject::with_prototype(
            Some(prototype.clone()),
            ObjectClass::Stream(handle),
        ))
    }

    fn stream_step(&self, function: StreamFunction) -> Value {
        self.native_function(native(function), "", 1)
    }

    pub(super) fn handle_of(value: &Value) -> Option<StreamHandle> {
        match value {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Stream(handle) => Some(handle.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn readable_of(&mut self, value: &Value) -> Result<Rc<RefCell<Readable>>, ScriptError> {
        match Self::handle_of(value) {
            Some(StreamHandle::Readable(stream)) => Ok(stream),
            _ => Err(self.type_error("Illegal invocation: not a ReadableStream")),
        }
    }

    // a stream with its controller, as both the constructor and the engine's bodies make it
    fn new_readable(
        &self,
        state: Readable,
        prototype: Option<ObjectHandle>,
    ) -> (Value, Rc<RefCell<Readable>>) {
        let stream = Rc::new(RefCell::new(state));
        let controller_prototype = self.intrinsics.streams.controller.clone();
        let controller = self.stream_object(
            &controller_prototype,
            StreamHandle::Controller(stream.clone()),
        );
        stream.borrow_mut().controller = Some(controller);
        let prototype = prototype.unwrap_or_else(|| self.intrinsics.streams.readable.clone());
        let object = self.stream_object(&prototype, StreamHandle::Readable(stream.clone()));
        (Value::Object(object), stream)
    }

    // a readable stream over a body's bytes, made once per body and then handed out again
    pub(super) fn body_stream(&self, holder: &ReadableStreamHandle) -> Value {
        if let Some(stream) = holder.borrow().object.clone() {
            return Value::Object(stream);
        }
        let body = holder.borrow().body.clone();
        let (value, stream) =
            self.new_readable(readable_state(Source::Body(body), 0.0, None), None);
        stream.borrow_mut().started = true;
        if let Value::Object(object) = &value {
            holder.borrow_mut().object = Some(object.clone());
        }
        value
    }

    // a readable stream over bytes, as Blob.stream hands out
    pub(super) fn bytes_stream(&self, bytes: Vec<u8>) -> Value {
        let state = readable_state(Source::Body(response_body(bytes)), 0.0, None);
        let (value, stream) = self.new_readable(state, None);
        stream.borrow_mut().started = true;
        value
    }

    // a stream the network feeds as a response body arrives, and the flag its cancel sets
    pub(super) fn network_stream(&self) -> (Value, Rc<RefCell<Readable>>, Rc<Cell<bool>>) {
        let canceled = Rc::new(Cell::new(false));
        let state = readable_state(Source::Network(canceled.clone()), 0.0, None);
        let (value, stream) = self.new_readable(state, None);
        stream.borrow_mut().started = true;
        (value, stream, canceled)
    }

    pub(super) fn network_chunk(
        &mut self,
        stream: &Rc<RefCell<Readable>>,
        bytes: Vec<u8>,
    ) -> Result<(), ScriptError> {
        if !matches!(stream.borrow().state, State::Readable) || stream.borrow().close_requested {
            return Ok(());
        }
        let length = bytes.len();
        let chunk = typed::new_typed(
            typed::ElementKind::Uint8,
            Rc::new(RefCell::new(bytes.into())),
            0,
            length,
        );
        self.enqueue_readable(stream, chunk)
    }

    pub(super) fn network_done(&mut self, stream: &Rc<RefCell<Readable>>) {
        if matches!(stream.borrow().state, State::Readable) && !stream.borrow().close_requested {
            self.close_requested(stream);
        }
    }

    pub(super) fn network_failed(&mut self, stream: &Rc<RefCell<Readable>>, reason: Value) {
        self.error_readable(stream, reason);
    }

    // the stream a body was made from when script gave one, rather than bytes
    pub(super) fn script_body(holder: &ReadableStreamHandle) -> Option<Rc<RefCell<Readable>>> {
        let object = holder.borrow().object.clone()?;
        let stream = match &object.borrow().class {
            ObjectClass::Stream(StreamHandle::Readable(stream)) => stream.clone(),
            _ => return None,
        };
        let own = matches!(&stream.borrow().source, Source::Body(body) if Rc::ptr_eq(body, &holder.borrow().body));
        (!own).then_some(stream)
    }

    // reads a stream body to its end and answers as the body method asked
    pub(super) fn collect_body(
        &mut self,
        stream: &Rc<RefCell<Readable>>,
        kind: BodyKind,
        mime_type: String,
    ) -> Result<Value, ScriptError> {
        if stream.borrow().reader.is_some() || stream.borrow().disturbed {
            return Ok(Value::Promise(
                self.rejected_promise("body stream already read"),
            ));
        }
        let reader = match self.acquire_reader(stream)? {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Stream(StreamHandle::Reader(reader)) => reader.clone(),
                _ => unreachable!("acquire_reader makes a reader"),
            },
            _ => unreachable!("acquire_reader makes a reader"),
        };
        let collect = Rc::new(Collect {
            reader,
            bytes: RefCell::new(Vec::new()),
            kind,
            mime_type,
            result: Self::pending_promise(),
        });
        self.collect_next(&collect)?;
        Ok(Value::Promise(collect.result.clone()))
    }

    fn collect_next(&mut self, collect: &Rc<Collect>) -> Result<(), ScriptError> {
        let read = self.read_from(&collect.reader)?;
        let fulfilled = self.stream_step(StreamFunction::Collected(collect.clone(), true));
        let rejected = self.stream_step(StreamFunction::Collected(collect.clone(), false));
        self.promise_then(read, Some(&fulfilled), Some(&rejected))?;
        Ok(())
    }

    fn collected(
        &mut self,
        collect: &Rc<Collect>,
        fulfilled: bool,
        result: Value,
    ) -> Result<(), ScriptError> {
        if !fulfilled {
            self.settle_promise(&collect.result, PromiseOutcome::Rejected(result));
            return Ok(());
        }
        if !self.get_property(&result, "done")?.truthy() {
            let chunk = self.get_property(&result, "value")?;
            if !matches!(chunk, Value::TypedArray(_) | Value::Bytes(_)) {
                let error = self.make_error("TypeError", "body stream chunks must be Uint8Array");
                self.release_reader(&collect.reader);
                self.settle_promise(&collect.result, PromiseOutcome::Rejected(error));
                return Ok(());
            }
            let bytes = self.buffer_source_bytes(&chunk)?;
            collect.bytes.borrow_mut().extend(bytes);
            return self.collect_next(collect);
        }
        self.release_reader(&collect.reader);
        let bytes = std::mem::take(&mut *collect.bytes.borrow_mut());
        let outcome = match collect.kind {
            BodyKind::Text => Ok(Value::String(
                String::from_utf8_lossy(&bytes).into_owned().into(),
            )),
            BodyKind::Json => self.parse_json(&String::from_utf8_lossy(&bytes)),
            BodyKind::Bytes => Ok(Value::Bytes(Rc::new(RefCell::new(bytes.into())))),
            BodyKind::Blob => Ok(Value::Blob(Rc::new(BlobData {
                bytes,
                mime_type: collect.mime_type.clone(),
            }))),
        };
        let outcome = match outcome {
            Ok(value) => PromiseOutcome::Fulfilled(value),
            Err(error) => PromiseOutcome::Rejected(self.take_thrown(error)),
        };
        self.settle_promise(&collect.result, outcome);
        Ok(())
    }

    pub(super) fn construct_stream(
        &mut self,
        function: &StreamFunction,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        match function {
            StreamFunction::Constructor => {
                let prototype = match &new_target {
                    Value::Object(_) => Some(self.prototype_from_constructor(&new_target)?),
                    _ => None,
                };
                self.construct_readable(first, arguments.get(1).cloned(), prototype)
            }
            StreamFunction::ReaderConstructor
                if matches!(Self::handle_of(&first), Some(StreamHandle::Readable(_))) =>
            {
                let stream = self.readable_of(&first)?;
                self.acquire_reader(&stream)
            }
            StreamFunction::ControllerConstructor
            | StreamFunction::WritableControllerConstructor
            | StreamFunction::TransformControllerConstructor
            | StreamFunction::ReaderConstructor => Err(self.type_error("Illegal constructor")),
            other => self.construct_writable_side(other, arguments, new_target),
        }
    }

    fn construct_readable(
        &mut self,
        source: Value,
        strategy: Option<Value>,
        prototype: Option<ObjectHandle>,
    ) -> Result<Value, ScriptError> {
        let source = match source {
            Value::Null => Value::Undefined,
            other => other,
        };
        if !matches!(source, Value::Undefined) && is_primitive(&source) {
            return Err(self.type_error("ReadableStream: the underlying source must be an object"));
        }
        let method = |runtime: &mut Self, name: &str| -> Result<Option<Value>, ScriptError> {
            if matches!(source, Value::Undefined) {
                return Ok(None);
            }
            match runtime.get_property(&source, name)? {
                Value::Undefined | Value::Null => Ok(None),
                found if is_callable(&found) => Ok(Some(found)),
                _ => Err(runtime.type_error(format!("ReadableStream: {name} must be a function"))),
            }
        };
        let start = method(self, "start")?;
        let pull = method(self, "pull")?;
        let cancel = method(self, "cancel")?;
        let (high_water_mark, size) = self.queuing_strategy(strategy, 1.0)?;
        let state = readable_state(
            Source::Script {
                object: source.clone(),
                pull,
                cancel,
            },
            high_water_mark,
            size,
        );
        let (value, stream) = self.new_readable(state, prototype);
        let controller = Value::Object(stream.borrow().controller.clone().expect("made with one"));
        let started = match start {
            Some(start) => self.call_function(start, source, vec![controller])?,
            None => Value::Undefined,
        };
        let promise = self.promise_resolve(started)?;
        let fulfilled = self.stream_step(StreamFunction::Started(stream.clone(), true));
        let rejected = self.stream_step(StreamFunction::Started(stream, false));
        self.promise_then(promise, Some(&fulfilled), Some(&rejected))?;
        Ok(value)
    }

    // the high-water mark and size function of a queuing strategy
    pub(super) fn queuing_strategy(
        &mut self,
        strategy: Option<Value>,
        default_mark: f64,
    ) -> Result<(f64, Option<Value>), ScriptError> {
        let Some(strategy) =
            strategy.filter(|value| !matches!(value, Value::Undefined | Value::Null))
        else {
            return Ok((default_mark, None));
        };
        let mark = match self.get_property(&strategy, "highWaterMark")? {
            Value::Undefined => default_mark,
            value => self.coerce_number(value)?,
        };
        if mark.is_nan() || mark < 0.0 {
            return Err(self.range_error("Invalid highWaterMark"));
        }
        let size = match self.get_property(&strategy, "size")? {
            Value::Undefined => None,
            found if is_callable(&found) => Some(found),
            _ => return Err(self.type_error("ReadableStream: size must be a function")),
        };
        Ok((mark, size))
    }

    fn acquire_reader(&mut self, stream: &Rc<RefCell<Readable>>) -> Result<Value, ScriptError> {
        if stream.borrow().reader.is_some() {
            return Err(self.type_error("ReadableStreamDefaultReader constructor can only accept readable streams that are not yet locked to a reader"));
        }
        let closed = Self::pending_promise();
        match &stream.borrow().state {
            State::Readable => {}
            State::Closed => {
                self.settle_promise(&closed, PromiseOutcome::Fulfilled(Value::Undefined))
            }
            State::Errored(reason) => {
                closed.borrow_mut().handled = true;
                self.settle_promise(&closed, PromiseOutcome::Rejected(reason.clone()));
            }
        }
        let reader = Rc::new(RefCell::new(Reader {
            stream: Some(stream.clone()),
            requests: VecDeque::new(),
            closed,
        }));
        stream.borrow_mut().reader = Some(reader.clone());
        if let Source::Body(body) = &stream.borrow().source {
            body.borrow_mut().locked = true;
        }
        let prototype = self.intrinsics.streams.reader.clone();
        Ok(Value::Object(
            self.stream_object(&prototype, StreamHandle::Reader(reader)),
        ))
    }

    fn desired_size(stream: &Readable) -> Option<f64> {
        match stream.state {
            State::Errored(_) => None,
            State::Closed => Some(0.0),
            State::Readable => Some(stream.high_water_mark - stream.queued),
        }
    }

    fn should_pull(stream: &Readable) -> bool {
        if !matches!(stream.state, State::Readable) || stream.close_requested || !stream.started {
            return false;
        }
        let waiting = stream
            .reader
            .as_ref()
            .is_some_and(|reader| !reader.borrow().requests.is_empty());
        waiting || Self::desired_size(stream).is_some_and(|size| size > 0.0)
    }

    fn call_pull_if_needed(&mut self, stream: &Rc<RefCell<Readable>>) -> Result<(), ScriptError> {
        if !Self::should_pull(&stream.borrow()) {
            return Ok(());
        }
        if stream.borrow().pulling {
            stream.borrow_mut().pull_again = true;
            return Ok(());
        }
        stream.borrow_mut().pulling = true;
        let pulled = self.pull_source(stream);
        match pulled {
            Ok(Some(promise)) => {
                let fulfilled = self.stream_step(StreamFunction::Pulled(stream.clone(), true));
                let rejected = self.stream_step(StreamFunction::Pulled(stream.clone(), false));
                self.promise_then(promise, Some(&fulfilled), Some(&rejected))?;
                Ok(())
            }
            Ok(None) => self.pull_settled(stream),
            Err(error) => {
                let reason = self.take_thrown(error);
                self.error_readable(stream, reason);
                Ok(())
            }
        }
    }

    // asks the source for more; a promise means the answer comes later
    fn pull_source(
        &mut self,
        stream: &Rc<RefCell<Readable>>,
    ) -> Result<Option<PromiseHandle>, ScriptError> {
        enum Pull {
            Script(Value, Value, Value),
            Body(ResponseBodyHandle),
            Iterator(Value, Value),
            Tee(Rc<TeeState>),
            Transform(Rc<TransformState>),
            Nothing,
        }
        let controller = stream.borrow().controller.clone().map(Value::Object);
        let pull = match &stream.borrow().source {
            Source::Script {
                object,
                pull: Some(pull),
                ..
            } => Pull::Script(
                pull.clone(),
                object.clone(),
                controller.unwrap_or(Value::Undefined),
            ),
            Source::Script { .. } => Pull::Nothing,
            Source::Body(body) => Pull::Body(body.clone()),
            Source::Iterator { iterator, next } => Pull::Iterator(iterator.clone(), next.clone()),
            Source::Tee(tee, _) => Pull::Tee(tee.clone()),
            Source::Transform(transform) => Pull::Transform(transform.clone()),
            Source::Network(_) => Pull::Nothing,
        };
        match pull {
            Pull::Nothing => Ok(None),
            Pull::Script(pull, object, controller) => {
                let result = self.call_function(pull, object, vec![controller])?;
                Ok(Some(self.promise_resolve(result)?))
            }
            Pull::Body(body) => {
                let chunk = {
                    let mut body = body.borrow_mut();
                    body.disturbed = true;
                    let start = body.cursor.min(body.bytes.len());
                    let end = start.saturating_add(BODY_CHUNK).min(body.bytes.len());
                    body.cursor = end;
                    if end == body.bytes.len() {
                        body.consumed = true;
                    }
                    (start < end).then(|| body.bytes[start..end].to_vec())
                };
                match chunk {
                    Some(chunk) => {
                        let length = chunk.len();
                        let bytes = typed::new_typed(
                            typed::ElementKind::Uint8,
                            Rc::new(RefCell::new(chunk.into())),
                            0,
                            length,
                        );
                        self.enqueue_readable(stream, bytes)?;
                    }
                    None => self.close_requested(stream),
                }
                Ok(None)
            }
            Pull::Iterator(iterator, next) => {
                let result = self.call_function(next, iterator, Vec::new())?;
                let promise = self.promise_resolve(result)?;
                let fulfilled = self.stream_step(StreamFunction::FromStep(stream.clone(), true));
                let rejected = self.stream_step(StreamFunction::FromStep(stream.clone(), false));
                let Value::Promise(after) =
                    self.promise_then(promise, Some(&fulfilled), Some(&rejected))?
                else {
                    unreachable!("promise_then hands back a promise")
                };
                Ok(Some(after))
            }
            Pull::Tee(tee) => {
                self.tee_pull(&tee)?;
                Ok(None)
            }
            Pull::Transform(transform) => {
                self.set_transform_backpressure(&transform, false);
                Ok(None)
            }
        }
    }

    fn pull_settled(&mut self, stream: &Rc<RefCell<Readable>>) -> Result<(), ScriptError> {
        let again = {
            let mut stream = stream.borrow_mut();
            stream.pulling = false;
            std::mem::take(&mut stream.pull_again)
        };
        if again {
            self.call_pull_if_needed(stream)?;
        }
        Ok(())
    }

    fn enqueue_readable(
        &mut self,
        stream: &Rc<RefCell<Readable>>,
        chunk: Value,
    ) -> Result<(), ScriptError> {
        if stream.borrow().close_requested || !matches!(stream.borrow().state, State::Readable) {
            return Err(self.type_error("Cannot enqueue a chunk into a readable stream that is closed or has been requested to be closed"));
        }
        let waiting = stream
            .borrow()
            .reader
            .as_ref()
            .and_then(|reader| reader.borrow_mut().requests.pop_front());
        if let Some(request) = waiting {
            self.settle_promise(
                &request,
                PromiseOutcome::Fulfilled(read_result(chunk, false)),
            );
        } else {
            let size = stream.borrow().size.clone();
            let size = match size {
                Some(size) => {
                    let measured = self
                        .call_function(size, Value::Undefined, vec![chunk.clone()])
                        .and_then(|value| self.coerce_number(value));
                    match measured {
                        Ok(size) if size.is_finite() && size >= 0.0 => size,
                        Ok(_) => {
                            let error = self.range_error("Invalid chunk size");
                            let reason = self.take_thrown(error);
                            self.error_readable(stream, reason.clone());
                            return Err(self.throw_value(reason));
                        }
                        Err(error) => {
                            let reason = self.take_thrown(error);
                            self.error_readable(stream, reason.clone());
                            return Err(self.throw_value(reason));
                        }
                    }
                }
                None => 1.0,
            };
            let mut stream = stream.borrow_mut();
            stream.queue.push_back((chunk, size));
            stream.queued += size;
        }
        self.call_pull_if_needed(stream)
    }

    fn close_requested(&mut self, stream: &Rc<RefCell<Readable>>) {
        let empty = {
            let mut stream = stream.borrow_mut();
            stream.close_requested = true;
            stream.queue.is_empty()
        };
        if empty {
            self.close_readable(stream);
        }
    }

    fn close_readable(&mut self, stream: &Rc<RefCell<Readable>>) {
        if !matches!(stream.borrow().state, State::Readable) {
            return;
        }
        stream.borrow_mut().state = State::Closed;
        let reader = stream.borrow().reader.clone();
        if let Some(reader) = reader {
            let (requests, closed) = {
                let mut reader = reader.borrow_mut();
                (std::mem::take(&mut reader.requests), reader.closed.clone())
            };
            for request in requests {
                self.settle_promise(
                    &request,
                    PromiseOutcome::Fulfilled(read_result(Value::Undefined, true)),
                );
            }
            self.settle_promise(&closed, PromiseOutcome::Fulfilled(Value::Undefined));
        }
    }

    fn error_readable(&mut self, stream: &Rc<RefCell<Readable>>, reason: Value) {
        if !matches!(stream.borrow().state, State::Readable) {
            return;
        }
        {
            let mut stream = stream.borrow_mut();
            stream.state = State::Errored(reason.clone());
            stream.queue.clear();
            stream.queued = 0.0;
        }
        let reader = stream.borrow().reader.clone();
        if let Some(reader) = reader {
            let (requests, closed) = {
                let mut reader = reader.borrow_mut();
                (std::mem::take(&mut reader.requests), reader.closed.clone())
            };
            for request in requests {
                self.settle_promise(&request, PromiseOutcome::Rejected(reason.clone()));
            }
            closed.borrow_mut().handled = true;
            self.settle_promise(&closed, PromiseOutcome::Rejected(reason));
        }
    }

    fn cancel_readable(
        &mut self,
        stream: &Rc<RefCell<Readable>>,
        reason: Value,
    ) -> Result<PromiseHandle, ScriptError> {
        stream.borrow_mut().disturbed = true;
        match stream.borrow().state.clone() {
            State::Closed => return Ok(self.fulfilled_promise(Value::Undefined)),
            State::Errored(error) => return Ok(self.rejected_promise_value(error)),
            State::Readable => {}
        }
        self.close_readable(stream);
        {
            let mut stream = stream.borrow_mut();
            stream.queue.clear();
            stream.queued = 0.0;
        }
        enum Cancel {
            Script(Value, Value),
            Iterator(Value),
            Tee(Rc<TeeState>, usize),
            Transform(Rc<TransformState>),
            Nothing,
        }
        let cancel = match &stream.borrow().source {
            Source::Script {
                object,
                cancel: Some(cancel),
                ..
            } => Cancel::Script(cancel.clone(), object.clone()),
            Source::Iterator { iterator, .. } => Cancel::Iterator(iterator.clone()),
            Source::Tee(tee, index) => Cancel::Tee(tee.clone(), *index),
            Source::Transform(transform) => Cancel::Transform(transform.clone()),
            Source::Network(canceled) => {
                canceled.set(true);
                Cancel::Nothing
            }
            Source::Body(body) => {
                let mut body = body.borrow_mut();
                body.cursor = body.bytes.len();
                body.consumed = true;
                Cancel::Nothing
            }
            Source::Script { .. } => Cancel::Nothing,
        };
        let outcome = match cancel {
            Cancel::Nothing => return Ok(self.fulfilled_promise(Value::Undefined)),
            Cancel::Script(cancel, object) => self.call_function(cancel, object, vec![reason]),
            Cancel::Iterator(iterator) => match self.get_property(&iterator, "return")? {
                Value::Undefined | Value::Null => Ok(Value::Undefined),
                close => self.call_function(close, iterator, vec![reason]),
            },
            Cancel::Tee(tee, index) => return self.tee_cancel(&tee, index, reason),
            Cancel::Transform(transform) => {
                self.transform_error_writable(&transform, reason.clone());
                self.transformer_cancel(&transform, reason)
            }
        };
        let promise = match outcome {
            Ok(value) => self.promise_resolve(value)?,
            Err(error) => {
                let thrown = self.take_thrown(error);
                return Ok(self.rejected_promise_value(thrown));
            }
        };
        let canceled = self.stream_step(StreamFunction::Canceled);
        match self.promise_then(promise, Some(&canceled), None)? {
            Value::Promise(promise) => Ok(promise),
            _ => unreachable!("promise_then hands back a promise"),
        }
    }

    fn read_from(&mut self, reader: &Rc<RefCell<Reader>>) -> Result<PromiseHandle, ScriptError> {
        let Some(stream) = reader.borrow().stream.clone() else {
            return Ok(self.rejected_promise("This readable stream reader has been released and cannot be used to read from its previous owner stream"));
        };
        stream.borrow_mut().disturbed = true;
        match stream.borrow().state.clone() {
            State::Closed => return Ok(self.fulfilled_promise(read_result(Value::Undefined, true))),
            State::Errored(reason) => return Ok(self.rejected_promise_value(reason)),
            State::Readable => {}
        }
        let queued = {
            let mut stream = stream.borrow_mut();
            let chunk = stream.queue.pop_front();
            if let Some((_, size)) = &chunk {
                stream.queued = (stream.queued - size).max(0.0);
            }
            chunk.map(|(chunk, _)| chunk)
        };
        if let Some(chunk) = queued {
            let drained = stream.borrow().close_requested && stream.borrow().queue.is_empty();
            if drained {
                self.close_readable(&stream);
            } else {
                self.call_pull_if_needed(&stream)?;
            }
            return Ok(self.fulfilled_promise(read_result(chunk, false)));
        }
        let request = Self::pending_promise();
        reader.borrow_mut().requests.push_back(request.clone());
        self.call_pull_if_needed(&stream)?;
        Ok(request)
    }

    fn release_reader(&mut self, reader: &Rc<RefCell<Reader>>) {
        let Some(stream) = reader.borrow_mut().stream.take() else {
            return;
        };
        let requests = std::mem::take(&mut reader.borrow_mut().requests);
        for request in requests {
            let reason = self.make_error("TypeError", "Releasing reader");
            self.settle_promise(&request, PromiseOutcome::Rejected(reason));
        }
        let reason = self.make_error("TypeError", "Releasing reader");
        let readable = matches!(stream.borrow().state, State::Readable);
        let closed = if readable {
            reader.borrow().closed.clone()
        } else {
            let replaced = Self::pending_promise();
            reader.borrow_mut().closed = replaced.clone();
            replaced
        };
        closed.borrow_mut().handled = true;
        self.settle_promise(&closed, PromiseOutcome::Rejected(reason));
        stream.borrow_mut().reader = None;
        if let Source::Body(body) = &stream.borrow().source {
            body.borrow_mut().locked = false;
        }
    }

    fn tee(&mut self, stream: &Rc<RefCell<Readable>>) -> Result<Value, ScriptError> {
        let reader = match self.acquire_reader(stream)? {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Stream(StreamHandle::Reader(reader)) => reader.clone(),
                _ => unreachable!("acquire_reader makes a reader"),
            },
            _ => unreachable!("acquire_reader makes a reader"),
        };
        let tee = Rc::new(TeeState {
            reader,
            original: stream.clone(),
            branches: RefCell::new(Vec::new()),
            reading: Cell::new(false),
            read_again: Cell::new(false),
            canceled: [Cell::new(false), Cell::new(false)],
            reasons: RefCell::new([Value::Undefined, Value::Undefined]),
            cancel_promise: Self::pending_promise(),
        });
        let mut values = Vec::new();
        for index in 0..2 {
            let (value, branch) = self.new_readable(
                readable_state(Source::Tee(tee.clone(), index), 1.0, None),
                None,
            );
            branch.borrow_mut().started = true;
            tee.branches.borrow_mut().push(branch);
            values.push(value);
        }
        Ok(Self::new_array(values))
    }

    fn tee_pull(&mut self, tee: &Rc<TeeState>) -> Result<(), ScriptError> {
        if tee.reading.get() {
            tee.read_again.set(true);
            return Ok(());
        }
        tee.reading.set(true);
        let read = self.read_from(&tee.reader)?;
        let fulfilled = self.stream_step(StreamFunction::TeeRead(tee.clone(), true));
        let rejected = self.stream_step(StreamFunction::TeeRead(tee.clone(), false));
        self.promise_then(read, Some(&fulfilled), Some(&rejected))?;
        Ok(())
    }

    fn tee_cancel(
        &mut self,
        tee: &Rc<TeeState>,
        index: usize,
        reason: Value,
    ) -> Result<PromiseHandle, ScriptError> {
        tee.canceled[index].set(true);
        tee.reasons.borrow_mut()[index] = reason;
        if tee.canceled[0].get() && tee.canceled[1].get() {
            let reasons = tee.reasons.borrow().to_vec();
            let original = tee.original.clone();
            let canceled = self.cancel_readable(&original, Self::new_array(reasons))?;
            let resolve = Value::Promise(canceled);
            self.resolve_promise(&tee.cancel_promise, resolve)?;
        }
        Ok(tee.cancel_promise.clone())
    }

    fn tee_answer(
        &mut self,
        tee: &Rc<TeeState>,
        fulfilled: bool,
        result: Value,
    ) -> Result<(), ScriptError> {
        let branches = tee.branches.borrow().clone();
        if !fulfilled {
            for branch in &branches {
                self.error_readable(branch, result.clone());
            }
            if !(tee.canceled[0].get() && tee.canceled[1].get()) {
                self.resolve_promise(&tee.cancel_promise, Value::Undefined)?;
            }
            return Ok(());
        }
        tee.reading.set(false);
        if self.get_property(&result, "done")?.truthy() {
            for (index, branch) in branches.iter().enumerate() {
                if !tee.canceled[index].get() {
                    self.close_requested(branch);
                }
            }
            if !(tee.canceled[0].get() && tee.canceled[1].get()) {
                self.resolve_promise(&tee.cancel_promise, Value::Undefined)?;
            }
            return Ok(());
        }
        let chunk = self.get_property(&result, "value")?;
        for (index, branch) in branches.iter().enumerate() {
            if !tee.canceled[index].get()
                && matches!(branch.borrow().state, State::Readable)
                && !branch.borrow().close_requested
            {
                self.enqueue_readable(branch, chunk.clone())?;
            }
        }
        if tee.read_again.replace(false) {
            self.tee_pull(tee)?;
        }
        Ok(())
    }

    fn stream_of_iterable(&mut self, source: Value) -> Result<Value, ScriptError> {
        if matches!(source, Value::Undefined | Value::Null) {
            return Err(self.type_error("ReadableStream.from requires an iterable"));
        }
        // a stream given to from is handed back as it is
        if let Some(StreamHandle::Readable(_)) = Self::handle_of(&source) {
            return Ok(source);
        }
        let method = self.get_property(&source, "@@asyncIterator")?;
        let iterator = if matches!(method, Value::Undefined | Value::Null) {
            let method = self.get_property(&source, "@@iterator")?;
            if !is_callable(&method) {
                return Err(self.type_error("ReadableStream.from: the argument is not iterable"));
            }
            self.call_function(method, source, Vec::new())?
        } else {
            self.call_function(method, source, Vec::new())?
        };
        if is_primitive(&iterator) {
            return Err(self.type_error("ReadableStream.from: the iterator is not an object"));
        }
        let next = self.get_property(&iterator, "next")?;
        let (value, stream) = self.new_readable(
            readable_state(Source::Iterator { iterator, next }, 0.0, None),
            None,
        );
        stream.borrow_mut().started = true;
        Ok(value)
    }

    // what the iterator answered; a promise handed back holds the pull until it settles
    fn iterable_answered(
        &mut self,
        stream: &Rc<RefCell<Readable>>,
        fulfilled: bool,
        result: Value,
    ) -> Result<Value, ScriptError> {
        if !fulfilled {
            self.error_readable(stream, result);
            return Ok(Value::Undefined);
        }
        if is_primitive(&result) {
            let error = self.type_error("The iterator result is not an object");
            let reason = self.take_thrown(error);
            self.error_readable(stream, reason);
            return Ok(Value::Undefined);
        }
        if self.get_property(&result, "done")?.truthy() {
            self.close_requested(stream);
            return Ok(Value::Undefined);
        }
        let value = self.get_property(&result, "value")?;
        // a sync iterator's values may be promises, which the stream waits for
        if matches!(value, Value::Promise(_)) {
            let settled = self.promise_resolve(value)?;
            let fulfilled = self.stream_step(StreamFunction::FromValue(stream.clone(), true));
            let rejected = self.stream_step(StreamFunction::FromValue(stream.clone(), false));
            return self.promise_then(settled, Some(&fulfilled), Some(&rejected));
        }
        self.enqueue_readable(stream, value)?;
        Ok(Value::Undefined)
    }

    pub(super) fn call_stream(
        &mut self,
        function: StreamFunction,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use StreamFunction as F;
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        match function {
            F::Constructor | F::ReaderConstructor | F::ControllerConstructor => Err(self.type_error(
                "Failed to construct: Please use the 'new' operator, this object constructor cannot be called as a function.",
            )),
            F::From => self.stream_of_iterable(first),
            F::Locked => {
                let stream = self.readable_of(&this)?;
                let locked = stream.borrow().reader.is_some();
                Ok(Value::Bool(locked))
            }
            F::Cancel => {
                let stream = match self.readable_of(&this) {
                    Ok(stream) => stream,
                    Err(error) => {
                        let reason = self.take_thrown(error);
                        return Ok(Value::Promise(self.rejected_promise_value(reason)));
                    }
                };
                if stream.borrow().reader.is_some() {
                    return Ok(Value::Promise(self.rejected_promise("Cannot cancel a stream that already has a reader")));
                }
                Ok(Value::Promise(self.cancel_readable(&stream, first)?))
            }
            F::GetReader => {
                let stream = self.readable_of(&this)?;
                if !matches!(first, Value::Undefined) && !is_primitive(&first) {
                    let mode = self.get_property(&first, "mode")?;
                    if !matches!(mode, Value::Undefined) {
                        let mode = self.coerce_string(mode)?;
                        if mode != "byob" {
                            return Err(self.type_error(format!("Failed to execute 'getReader' on 'ReadableStream': The provided value '{mode}' is not a valid enum value of type ReadableStreamReaderMode.")));
                        }
                        return Err(self.type_error("Cannot use a BYOB reader with a non-byte stream"));
                    }
                }
                self.acquire_reader(&stream)
            }
            F::Tee => {
                let stream = self.readable_of(&this)?;
                self.tee(&stream)
            }
            F::Values => {
                let stream = self.readable_of(&this)?;
                let prevent_cancel = if is_primitive(&first) {
                    false
                } else {
                    self.get_property(&first, "preventCancel")?.truthy()
                };
                let reader = match self.acquire_reader(&stream)? {
                    Value::Object(object) => match &object.borrow().class {
                        ObjectClass::Stream(StreamHandle::Reader(reader)) => reader.clone(),
                        _ => unreachable!("acquire_reader makes a reader"),
                    },
                    _ => unreachable!("acquire_reader makes a reader"),
                };
                let iteration = Rc::new(RefCell::new(StreamIteration {
                    reader,
                    prevent_cancel,
                    done: false,
                }));
                let prototype = self.intrinsics.streams.iterator.clone();
                Ok(Value::Object(self.stream_object(&prototype, StreamHandle::Iterator(iteration))))
            }
            F::DesiredSize | F::Enqueue | F::Close | F::Error => {
                let Some(StreamHandle::Controller(stream)) = Self::handle_of(&this) else {
                    return Err(self.type_error("Illegal invocation: not a ReadableStreamDefaultController"));
                };
                match function {
                    F::DesiredSize => Ok(Self::desired_size(&stream.borrow()).map_or(Value::Null, Value::Number)),
                    F::Enqueue => {
                        self.enqueue_readable(&stream, first)?;
                        Ok(Value::Undefined)
                    }
                    F::Close => {
                        if stream.borrow().close_requested || !matches!(stream.borrow().state, State::Readable) {
                            return Err(self.type_error("Cannot close a readable stream that has already been requested to be closed"));
                        }
                        self.close_requested(&stream);
                        Ok(Value::Undefined)
                    }
                    _ => {
                        self.error_readable(&stream, first);
                        Ok(Value::Undefined)
                    }
                }
            }
            F::Read | F::ReleaseLock | F::ReaderCancel | F::Closed => {
                let Some(StreamHandle::Reader(reader)) = Self::handle_of(&this) else {
                    let error = self.type_error("Illegal invocation: not a ReadableStreamDefaultReader");
                    if matches!(function, F::ReleaseLock) {
                        return Err(error);
                    }
                    let reason = self.take_thrown(error);
                    return Ok(Value::Promise(self.rejected_promise_value(reason)));
                };
                match function {
                    F::Read => Ok(Value::Promise(self.read_from(&reader)?)),
                    F::ReleaseLock => {
                        self.release_reader(&reader);
                        Ok(Value::Undefined)
                    }
                    F::Closed => Ok(Value::Promise(reader.borrow().closed.clone())),
                    _ => {
                        let Some(stream) = reader.borrow().stream.clone() else {
                            return Ok(Value::Promise(self.rejected_promise("This readable stream reader has been released")));
                        };
                        Ok(Value::Promise(self.cancel_readable(&stream, first)?))
                    }
                }
            }
            F::IteratorNext | F::IteratorReturn => {
                let Some(StreamHandle::Iterator(iteration)) = Self::handle_of(&this) else {
                    return Ok(Value::Promise(self.rejected_promise("Illegal invocation: not a stream iterator")));
                };
                if iteration.borrow().done {
                    let value = if matches!(function, F::IteratorReturn) { first } else { Value::Undefined };
                    return Ok(Value::Promise(self.fulfilled_promise(read_result(value, true))));
                }
                let reader = iteration.borrow().reader.clone();
                if matches!(function, F::IteratorNext) {
                    let read = self.read_from(&reader)?;
                    let step = self.stream_step(StreamFunction::IterationRead(iteration.clone()));
                    return self.promise_then(read, Some(&step), None);
                }
                iteration.borrow_mut().done = true;
                let prevent_cancel = iteration.borrow().prevent_cancel;
                let stream = reader.borrow().stream.clone();
                let canceled = match stream {
                    Some(stream) if !prevent_cancel => Some(self.cancel_readable(&stream, first.clone())?),
                    _ => None,
                };
                self.release_reader(&reader);
                match canceled {
                    Some(canceled) => {
                        let done = self.stream_step(StreamFunction::ReturnDone(first));
                        self.promise_then(canceled, Some(&done), None)
                    }
                    None => Ok(Value::Promise(self.fulfilled_promise(read_result(first, true)))),
                }
            }
            F::Started(stream, fulfilled) => {
                if fulfilled {
                    stream.borrow_mut().started = true;
                    self.call_pull_if_needed(&stream)?;
                } else {
                    self.error_readable(&stream, first);
                }
                Ok(Value::Undefined)
            }
            F::Pulled(stream, fulfilled) => {
                if fulfilled {
                    self.pull_settled(&stream)?;
                } else {
                    stream.borrow_mut().pulling = false;
                    self.error_readable(&stream, first);
                }
                Ok(Value::Undefined)
            }
            F::Canceled => Ok(Value::Undefined),
            F::FromStep(stream, fulfilled) => self.iterable_answered(&stream, fulfilled, first),
            F::FromValue(stream, fulfilled) => {
                if fulfilled {
                    self.enqueue_readable(&stream, first)?;
                } else {
                    self.error_readable(&stream, first);
                }
                Ok(Value::Undefined)
            }
            F::ReturnDone(value) => Ok(read_result(value, true)),
            F::TeeRead(tee, fulfilled) => {
                self.tee_answer(&tee, fulfilled, first)?;
                Ok(Value::Undefined)
            }
            F::IterationRead(iteration) => {
                if self.get_property(&first, "done")?.truthy() {
                    iteration.borrow_mut().done = true;
                    let reader = iteration.borrow().reader.clone();
                    self.release_reader(&reader);
                }
                Ok(first)
            }
            other => self.call_writable_stream(other, this, arguments),
        }
    }
}

// ---- writable and transform streams, pipes, and the streams built on them ----

#[derive(Clone)]
enum WriteState {
    Writable,
    Erroring(Value),
    Errored(Value),
    Closed,
}

enum WriteItem {
    Chunk(Value, f64, PromiseHandle),
    Close(PromiseHandle),
}

enum Sink {
    Script {
        object: Value,
        write: Option<Value>,
        close: Option<Value>,
        abort: Option<Value>,
    },
    Transform(Rc<TransformState>),
}

pub(super) struct Writable {
    state: WriteState,
    queue: VecDeque<WriteItem>,
    queued: f64,
    high_water_mark: f64,
    size: Option<Value>,
    sink: Sink,
    started: bool,
    in_flight: bool,
    close_requested: bool,
    pending_abort: Option<(Value, PromiseHandle)>,
    writer: Option<Rc<RefCell<Writer>>>,
    backpressure: bool,
    controller: Option<ObjectHandle>,
    signal: AbortSignalHandle,
}

pub(super) struct Writer {
    stream: Option<Rc<RefCell<Writable>>>,
    ready: PromiseHandle,
    closed: PromiseHandle,
}

pub(super) enum Transformer {
    Script {
        object: Value,
        transform: Option<Value>,
        flush: Option<Value>,
        cancel: Option<Value>,
    },
    // TextEncoderStream: strings to UTF-8, holding a high surrogate split from its pair
    Encode(RefCell<Option<u16>>),
    // TextDecoderStream: a TextDecoder run in streaming mode
    Decode(Value),
    // CompressionStream and DecompressionStream gather the input and code it at the end
    Compress {
        format: Compression,
        decompress: bool,
        input: RefCell<Vec<u8>>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Compression {
    Gzip,
    Deflate,
    DeflateRaw,
}

pub(super) struct TransformState {
    readable: RefCell<Option<Rc<RefCell<Readable>>>>,
    writable: RefCell<Option<Rc<RefCell<Writable>>>>,
    transformer: Transformer,
    controller: RefCell<Option<ObjectHandle>>,
    backpressure: Cell<bool>,
    backpressure_change: RefCell<Option<PromiseHandle>>,
}

pub(super) struct Pipe {
    reader: Rc<RefCell<Reader>>,
    writer: Rc<RefCell<Writer>>,
    source: Rc<RefCell<Readable>>,
    dest: Rc<RefCell<Writable>>,
    prevent_close: bool,
    prevent_abort: bool,
    prevent_cancel: bool,
    result: PromiseHandle,
    done: Cell<bool>,
}

// the readable and writable sides of a stream pair such as TextEncoderStream
pub(super) struct Pair {
    readable: Value,
    writable: Value,
    kind: PairKind,
}

#[derive(Clone)]
pub(super) enum PairKind {
    Transform,
    Encoder,
    Decoder(Value),
    Compression,
}

fn write_state(sink: Sink, high_water_mark: f64, size: Option<Value>) -> Writable {
    Writable {
        state: WriteState::Writable,
        queue: VecDeque::new(),
        queued: 0.0,
        high_water_mark,
        size,
        sink,
        started: false,
        in_flight: false,
        close_requested: false,
        pending_abort: None,
        writer: None,
        backpressure: false,
        controller: None,
        signal: new_abort_signal(),
    }
}

fn gunzip(data: &[u8]) -> Option<Vec<u8>> {
    let [0x1f, 0x8b, 8, flags, ..] = *data else {
        return None;
    };
    let mut at = 10;
    if flags & 4 != 0 {
        let extra = usize::from(u16::from_le_bytes([*data.get(at)?, *data.get(at + 1)?]));
        at += 2 + extra;
    }
    for flag in [8, 16] {
        if flags & flag != 0 {
            at += data.get(at..)?.iter().position(|&byte| byte == 0)? + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    crate::inflate::inflate(data.get(at..)?, MAX_INFLATED_BYTES)
        .ok()
        .map(|(bytes, _)| bytes)
}

const MAX_INFLATED_BYTES: usize = 256 * 1024 * 1024;

impl ScriptRuntime {
    fn writable_of(&mut self, value: &Value) -> Result<Rc<RefCell<Writable>>, ScriptError> {
        match Self::handle_of(value) {
            Some(StreamHandle::Writable(stream)) => Ok(stream),
            _ => Err(self.type_error("Illegal invocation: not a WritableStream")),
        }
    }

    fn new_writable(
        &self,
        state: Writable,
        prototype: Option<ObjectHandle>,
    ) -> (Value, Rc<RefCell<Writable>>) {
        let stream = Rc::new(RefCell::new(state));
        let controller_prototype = self.intrinsics.streams.writable_controller.clone();
        let controller = self.stream_object(
            &controller_prototype,
            StreamHandle::WritableController(stream.clone()),
        );
        stream.borrow_mut().controller = Some(controller);
        let prototype = prototype.unwrap_or_else(|| self.intrinsics.streams.writable.clone());
        let object = self.stream_object(&prototype, StreamHandle::Writable(stream.clone()));
        (Value::Object(object), stream)
    }

    fn construct_writable(
        &mut self,
        sink: Value,
        strategy: Option<Value>,
        prototype: Option<ObjectHandle>,
    ) -> Result<Value, ScriptError> {
        let sink = match sink {
            Value::Null => Value::Undefined,
            other => other,
        };
        if !matches!(sink, Value::Undefined) && is_primitive(&sink) {
            return Err(self.type_error("WritableStream: the underlying sink must be an object"));
        }
        let method = |runtime: &mut Self, name: &str| -> Result<Option<Value>, ScriptError> {
            if matches!(sink, Value::Undefined) {
                return Ok(None);
            }
            match runtime.get_property(&sink, name)? {
                Value::Undefined | Value::Null => Ok(None),
                found if is_callable(&found) => Ok(Some(found)),
                _ => Err(runtime.type_error(format!("WritableStream: {name} must be a function"))),
            }
        };
        let start = method(self, "start")?;
        let write = method(self, "write")?;
        let close = method(self, "close")?;
        let abort = method(self, "abort")?;
        if !matches!(sink, Value::Undefined)
            && !matches!(self.get_property(&sink, "type")?, Value::Undefined)
        {
            return Err(self.range_error("WritableStream: invalid type"));
        }
        let (high_water_mark, size) = self.queuing_strategy(strategy, 1.0)?;
        let state = write_state(
            Sink::Script {
                object: sink.clone(),
                write,
                close,
                abort,
            },
            high_water_mark,
            size,
        );
        let (value, stream) = self.new_writable(state, prototype);
        self.update_backpressure(&stream);
        let controller = Value::Object(stream.borrow().controller.clone().expect("made with one"));
        let started = match start {
            Some(start) => self.call_function(start, sink, vec![controller])?,
            None => Value::Undefined,
        };
        self.start_writable(&stream, started)?;
        Ok(value)
    }

    fn start_writable(
        &mut self,
        stream: &Rc<RefCell<Writable>>,
        started: Value,
    ) -> Result<(), ScriptError> {
        let promise = self.promise_resolve(started)?;
        let fulfilled = self.stream_step(StreamFunction::WriterStarted(stream.clone(), true));
        let rejected = self.stream_step(StreamFunction::WriterStarted(stream.clone(), false));
        self.promise_then(promise, Some(&fulfilled), Some(&rejected))?;
        Ok(())
    }

    fn writable_desired_size(stream: &Writable) -> Option<f64> {
        match stream.state {
            WriteState::Errored(_) | WriteState::Erroring(_) => None,
            WriteState::Closed => Some(0.0),
            WriteState::Writable => Some(stream.high_water_mark - stream.queued),
        }
    }

    fn update_backpressure(&mut self, stream: &Rc<RefCell<Writable>>) {
        let (backpressure, changed, writer) = {
            let mut stream = stream.borrow_mut();
            let backpressure = matches!(stream.state, WriteState::Writable)
                && !stream.close_requested
                && Self::writable_desired_size(&stream).is_some_and(|size| size <= 0.0);
            let changed = backpressure != stream.backpressure;
            stream.backpressure = backpressure;
            (backpressure, changed, stream.writer.clone())
        };
        if !changed {
            return;
        }
        if let Some(writer) = writer {
            if backpressure {
                writer.borrow_mut().ready = Self::pending_promise();
            } else {
                let ready = writer.borrow().ready.clone();
                self.settle_promise(&ready, PromiseOutcome::Fulfilled(Value::Undefined));
            }
        }
    }

    fn write_chunk(
        &mut self,
        stream: &Rc<RefCell<Writable>>,
        chunk: Value,
    ) -> Result<PromiseHandle, ScriptError> {
        let size = stream.borrow().size.clone();
        let size = match size {
            Some(size) => {
                let measured = self
                    .call_function(size, Value::Undefined, vec![chunk.clone()])
                    .and_then(|value| self.coerce_number(value));
                match measured {
                    Ok(size) if size.is_finite() && size >= 0.0 => size,
                    Ok(_) => {
                        let error = self.range_error("Invalid chunk size");
                        let reason = self.take_thrown(error);
                        self.controller_error(stream, reason.clone());
                        return Ok(self.rejected_promise_value(reason));
                    }
                    Err(error) => {
                        let reason = self.take_thrown(error);
                        self.controller_error(stream, reason.clone());
                        return Ok(self.rejected_promise_value(reason));
                    }
                }
            }
            None => 1.0,
        };
        match stream.borrow().state.clone() {
            WriteState::Errored(reason) | WriteState::Erroring(reason) => {
                return Ok(self.rejected_promise_value(reason));
            }
            WriteState::Closed => {
                return Ok(self.rejected_promise("Cannot write to a closed stream"));
            }
            WriteState::Writable => {}
        }
        if stream.borrow().close_requested {
            return Ok(self.rejected_promise("Cannot write to a stream that is closing"));
        }
        let promise = Self::pending_promise();
        {
            let mut stream = stream.borrow_mut();
            stream
                .queue
                .push_back(WriteItem::Chunk(chunk, size, promise.clone()));
            stream.queued += size;
        }
        self.update_backpressure(stream);
        self.advance_writable(stream)?;
        Ok(promise)
    }

    fn close_writable(
        &mut self,
        stream: &Rc<RefCell<Writable>>,
    ) -> Result<PromiseHandle, ScriptError> {
        if matches!(
            stream.borrow().state,
            WriteState::Closed | WriteState::Errored(_)
        ) || stream.borrow().close_requested
        {
            return Ok(
                self.rejected_promise("Cannot close a stream that is closed, errored or closing")
            );
        }
        let promise = Self::pending_promise();
        let (writer, writable, backpressure) = {
            let mut stream = stream.borrow_mut();
            stream.close_requested = true;
            stream.queue.push_back(WriteItem::Close(promise.clone()));
            (
                stream.writer.clone(),
                matches!(stream.state, WriteState::Writable),
                stream.backpressure,
            )
        };
        if let Some(writer) = writer
            && backpressure
            && writable
        {
            let ready = writer.borrow().ready.clone();
            self.settle_promise(&ready, PromiseOutcome::Fulfilled(Value::Undefined));
        }
        self.advance_writable(stream)?;
        Ok(promise)
    }

    fn advance_writable(&mut self, stream: &Rc<RefCell<Writable>>) -> Result<(), ScriptError> {
        if !stream.borrow().started || stream.borrow().in_flight {
            return Ok(());
        }
        if matches!(stream.borrow().state, WriteState::Erroring(_)) {
            self.finish_erroring(stream)?;
            return Ok(());
        }
        let front = match stream.borrow().queue.front() {
            None => return Ok(()),
            Some(WriteItem::Close(_)) => None,
            Some(WriteItem::Chunk(chunk, _, _)) => Some(chunk.clone()),
        };
        stream.borrow_mut().in_flight = true;
        let closing = front.is_none();
        let outcome = self.call_sink(stream, front);
        let promise = match outcome {
            Ok(value) => self.promise_resolve(value)?,
            Err(error) => {
                let reason = self.take_thrown(error);
                self.rejected_promise_value(reason)
            }
        };
        let fulfilled = self.stream_step(StreamFunction::Wrote(stream.clone(), closing, true));
        let rejected = self.stream_step(StreamFunction::Wrote(stream.clone(), closing, false));
        self.promise_then(promise, Some(&fulfilled), Some(&rejected))?;
        Ok(())
    }

    // the sink's write of a chunk, or its close when there is none
    fn call_sink(
        &mut self,
        stream: &Rc<RefCell<Writable>>,
        chunk: Option<Value>,
    ) -> Result<Value, ScriptError> {
        enum Call {
            Script(Option<Value>, Value),
            Transform(Rc<TransformState>),
        }
        let controller = stream
            .borrow()
            .controller
            .clone()
            .map_or(Value::Undefined, Value::Object);
        let call = match &stream.borrow().sink {
            Sink::Script {
                object,
                write,
                close,
                ..
            } => Call::Script(
                if chunk.is_some() {
                    write.clone()
                } else {
                    close.clone()
                },
                object.clone(),
            ),
            Sink::Transform(transform) => Call::Transform(transform.clone()),
        };
        match call {
            Call::Script(None, _) => Ok(Value::Undefined),
            Call::Script(Some(method), object) => {
                let mut arguments = Vec::new();
                if let Some(chunk) = chunk {
                    arguments.push(chunk);
                    arguments.push(controller);
                }
                self.call_function(method, object, arguments)
            }
            Call::Transform(transform) => match chunk {
                Some(chunk) => self.transform_write(&transform, chunk),
                None => self.transform_close(&transform),
            },
        }
    }

    fn wrote(
        &mut self,
        stream: &Rc<RefCell<Writable>>,
        closing: bool,
        fulfilled: bool,
        reason: Value,
    ) -> Result<(), ScriptError> {
        let item = {
            let mut stream = stream.borrow_mut();
            stream.in_flight = false;
            let item = stream.queue.pop_front();
            if let Some(WriteItem::Chunk(_, size, _)) = &item {
                stream.queued = (stream.queued - size).max(0.0);
            }
            item
        };
        let promise = match item {
            Some(WriteItem::Chunk(_, _, promise) | WriteItem::Close(promise)) => Some(promise),
            None => None,
        };
        if !fulfilled {
            if let Some(promise) = promise {
                self.settle_promise(&promise, PromiseOutcome::Rejected(reason.clone()));
            }
            if matches!(stream.borrow().state, WriteState::Writable) {
                self.start_erroring(stream, reason);
            } else if matches!(stream.borrow().state, WriteState::Erroring(_)) {
                self.finish_erroring(stream)?;
            }
            return Ok(());
        }
        if let Some(promise) = promise {
            self.settle_promise(&promise, PromiseOutcome::Fulfilled(Value::Undefined));
        }
        if closing {
            let (writer, abort) = {
                let mut stream = stream.borrow_mut();
                stream.state = WriteState::Closed;
                (stream.writer.clone(), stream.pending_abort.take())
            };
            if let Some((_, abort)) = abort {
                self.settle_promise(&abort, PromiseOutcome::Fulfilled(Value::Undefined));
            }
            if let Some(writer) = writer {
                let closed = writer.borrow().closed.clone();
                self.settle_promise(&closed, PromiseOutcome::Fulfilled(Value::Undefined));
            }
            return Ok(());
        }
        if matches!(stream.borrow().state, WriteState::Erroring(_)) {
            return self.finish_erroring(stream);
        }
        self.update_backpressure(stream);
        self.advance_writable(stream)
    }

    fn controller_error(&mut self, stream: &Rc<RefCell<Writable>>, reason: Value) {
        if matches!(stream.borrow().state, WriteState::Writable) {
            self.start_erroring(stream, reason);
        }
    }

    fn start_erroring(&mut self, stream: &Rc<RefCell<Writable>>, reason: Value) {
        let writer = {
            let mut stream = stream.borrow_mut();
            stream.state = WriteState::Erroring(reason.clone());
            stream.writer.clone()
        };
        if let Some(writer) = writer {
            let ready = Self::pending_promise();
            ready.borrow_mut().handled = true;
            self.settle_promise(&ready, PromiseOutcome::Rejected(reason.clone()));
            let previous = std::mem::replace(&mut writer.borrow_mut().ready, ready);
            previous.borrow_mut().handled = true;
            self.settle_promise(&previous, PromiseOutcome::Rejected(reason));
        }
        if stream.borrow().started && !stream.borrow().in_flight {
            let _ = self.finish_erroring(stream);
        }
    }

    fn finish_erroring(&mut self, stream: &Rc<RefCell<Writable>>) -> Result<(), ScriptError> {
        let WriteState::Erroring(reason) = stream.borrow().state.clone() else {
            return Ok(());
        };
        let (items, abort, writer) = {
            let mut stream = stream.borrow_mut();
            stream.state = WriteState::Errored(reason.clone());
            stream.queued = 0.0;
            (
                std::mem::take(&mut stream.queue),
                stream.pending_abort.take(),
                stream.writer.clone(),
            )
        };
        for item in items {
            let (WriteItem::Chunk(_, _, promise) | WriteItem::Close(promise)) = item;
            self.settle_promise(&promise, PromiseOutcome::Rejected(reason.clone()));
        }
        if let Some(writer) = writer {
            let closed = writer.borrow().closed.clone();
            closed.borrow_mut().handled = true;
            self.settle_promise(&closed, PromiseOutcome::Rejected(reason.clone()));
        }
        let Some((abort_reason, abort)) = abort else {
            return Ok(());
        };
        enum Abort {
            Script(Value, Value),
            Transform(Rc<TransformState>),
            Nothing,
        }
        let call = match &stream.borrow().sink {
            Sink::Script {
                object,
                abort: Some(method),
                ..
            } => Abort::Script(method.clone(), object.clone()),
            Sink::Transform(transform) => Abort::Transform(transform.clone()),
            Sink::Script { .. } => Abort::Nothing,
        };
        let outcome = match call {
            Abort::Nothing => Ok(Value::Undefined),
            Abort::Script(method, object) => self.call_function(method, object, vec![abort_reason]),
            Abort::Transform(transform) => {
                let readable = transform.readable.borrow().clone();
                if let Some(readable) = readable {
                    self.error_readable(&readable, abort_reason.clone());
                }
                self.transformer_cancel(&transform, abort_reason)
            }
        };
        match outcome {
            Ok(value) => {
                let promise = self.promise_resolve(value)?;
                self.resolve_promise(&abort, Value::Promise(promise))?;
            }
            Err(error) => {
                let thrown = self.take_thrown(error);
                self.settle_promise(&abort, PromiseOutcome::Rejected(thrown));
            }
        }
        Ok(())
    }

    fn abort_writable(
        &mut self,
        stream: &Rc<RefCell<Writable>>,
        reason: Value,
    ) -> Result<PromiseHandle, ScriptError> {
        if matches!(
            stream.borrow().state,
            WriteState::Closed | WriteState::Errored(_)
        ) {
            return Ok(self.fulfilled_promise(Value::Undefined));
        }
        let signal = stream.borrow().signal.clone();
        self.abort_signal(signal, reason.clone())?;
        if matches!(
            stream.borrow().state,
            WriteState::Closed | WriteState::Errored(_)
        ) {
            return Ok(self.fulfilled_promise(Value::Undefined));
        }
        if let Some((_, pending)) = &stream.borrow().pending_abort {
            return Ok(pending.clone());
        }
        let promise = Self::pending_promise();
        let erroring = matches!(stream.borrow().state, WriteState::Erroring(_));
        stream.borrow_mut().pending_abort = Some((reason.clone(), promise.clone()));
        if !erroring {
            self.start_erroring(stream, reason);
        }
        Ok(promise)
    }

    fn acquire_writer(&mut self, stream: &Rc<RefCell<Writable>>) -> Result<Value, ScriptError> {
        if stream.borrow().writer.is_some() {
            return Err(self.type_error("Cannot create writer when WritableStream is locked"));
        }
        let ready = Self::pending_promise();
        let closed = Self::pending_promise();
        let state = stream.borrow().state.clone();
        match state {
            WriteState::Writable => {
                if !stream.borrow().backpressure || stream.borrow().close_requested {
                    self.settle_promise(&ready, PromiseOutcome::Fulfilled(Value::Undefined));
                }
            }
            WriteState::Erroring(reason) => {
                ready.borrow_mut().handled = true;
                self.settle_promise(&ready, PromiseOutcome::Rejected(reason));
            }
            WriteState::Closed => {
                self.settle_promise(&ready, PromiseOutcome::Fulfilled(Value::Undefined));
                self.settle_promise(&closed, PromiseOutcome::Fulfilled(Value::Undefined));
            }
            WriteState::Errored(reason) => {
                for promise in [&ready, &closed] {
                    promise.borrow_mut().handled = true;
                    self.settle_promise(promise, PromiseOutcome::Rejected(reason.clone()));
                }
            }
        }
        let writer = Rc::new(RefCell::new(Writer {
            stream: Some(stream.clone()),
            ready,
            closed,
        }));
        stream.borrow_mut().writer = Some(writer.clone());
        let prototype = self.intrinsics.streams.writer.clone();
        Ok(Value::Object(
            self.stream_object(&prototype, StreamHandle::Writer(writer)),
        ))
    }

    fn release_writer(&mut self, writer: &Rc<RefCell<Writer>>) {
        let Some(stream) = writer.borrow_mut().stream.take() else {
            return;
        };
        for slot in 0..2 {
            let reason = self.make_error("TypeError", "Writer was released");
            let promise = Self::pending_promise();
            promise.borrow_mut().handled = true;
            self.settle_promise(&promise, PromiseOutcome::Rejected(reason.clone()));
            let previous = {
                let mut writer = writer.borrow_mut();
                let target = if slot == 0 {
                    &mut writer.ready
                } else {
                    &mut writer.closed
                };
                std::mem::replace(target, promise)
            };
            previous.borrow_mut().handled = true;
            self.settle_promise(&previous, PromiseOutcome::Rejected(reason));
        }
        stream.borrow_mut().writer = None;
    }

    fn writer_of(&mut self, this: &Value) -> Option<Rc<RefCell<Writer>>> {
        match Self::handle_of(this) {
            Some(StreamHandle::Writer(writer)) => Some(writer),
            _ => None,
        }
    }

    // ---- transforms ----

    fn construct_transform(
        &mut self,
        transformer: Value,
        writable_strategy: Option<Value>,
        readable_strategy: Option<Value>,
        prototype: Option<ObjectHandle>,
    ) -> Result<Value, ScriptError> {
        let transformer = match transformer {
            Value::Null => Value::Undefined,
            other => other,
        };
        if !matches!(transformer, Value::Undefined) && is_primitive(&transformer) {
            return Err(self.type_error("TransformStream: the transformer must be an object"));
        }
        let method = |runtime: &mut Self, name: &str| -> Result<Option<Value>, ScriptError> {
            if matches!(transformer, Value::Undefined) {
                return Ok(None);
            }
            match runtime.get_property(&transformer, name)? {
                Value::Undefined | Value::Null => Ok(None),
                found if is_callable(&found) => Ok(Some(found)),
                _ => Err(runtime.type_error(format!("TransformStream: {name} must be a function"))),
            }
        };
        let start = method(self, "start")?;
        let transform = method(self, "transform")?;
        let flush = method(self, "flush")?;
        let cancel = method(self, "cancel")?;
        for side in ["readableType", "writableType"] {
            if !matches!(transformer, Value::Undefined)
                && !matches!(self.get_property(&transformer, side)?, Value::Undefined)
            {
                return Err(self.range_error(format!("TransformStream: invalid {side}")));
            }
        }
        let (writable_mark, writable_size) = self.queuing_strategy(writable_strategy, 1.0)?;
        let (readable_mark, readable_size) = self.queuing_strategy(readable_strategy, 0.0)?;
        let (pair, state) = self.new_transform(
            Transformer::Script {
                object: transformer.clone(),
                transform,
                flush,
                cancel,
            },
            (writable_mark, writable_size),
            (readable_mark, readable_size),
            prototype,
            PairKind::Transform,
        );
        let controller = state
            .controller
            .borrow()
            .clone()
            .map_or(Value::Undefined, Value::Object);
        let started = match start {
            Some(start) => self.call_function(start, transformer, vec![controller])?,
            None => Value::Undefined,
        };
        let promise = self.promise_resolve(started)?;
        let readable = state.readable.borrow().clone().expect("made with one");
        let writable = state.writable.borrow().clone().expect("made with one");
        let started = Value::Promise(promise.clone());
        self.start_writable(&writable, started.clone())?;
        let fulfilled = self.stream_step(StreamFunction::Started(readable.clone(), true));
        let rejected = self.stream_step(StreamFunction::Started(readable, false));
        self.promise_then(promise, Some(&fulfilled), Some(&rejected))?;
        Ok(pair)
    }

    // a transform's two sides and the object holding them
    fn new_transform(
        &mut self,
        transformer: Transformer,
        writable: (f64, Option<Value>),
        readable: (f64, Option<Value>),
        prototype: Option<ObjectHandle>,
        kind: PairKind,
    ) -> (Value, Rc<TransformState>) {
        let state = Rc::new(TransformState {
            readable: RefCell::new(None),
            writable: RefCell::new(None),
            transformer,
            controller: RefCell::new(None),
            backpressure: Cell::new(false),
            backpressure_change: RefCell::new(None),
        });
        let (readable_value, readable_stream) = self.new_readable(
            readable_state(Source::Transform(state.clone()), readable.0, readable.1),
            None,
        );
        let (writable_value, writable_stream) = self.new_writable(
            write_state(Sink::Transform(state.clone()), writable.0, writable.1),
            None,
        );
        *state.readable.borrow_mut() = Some(readable_stream);
        *state.writable.borrow_mut() = Some(writable_stream.clone());
        self.update_backpressure(&writable_stream);
        let controller_prototype = self.intrinsics.streams.transform_controller.clone();
        let controller = self.stream_object(
            &controller_prototype,
            StreamHandle::TransformController(state.clone()),
        );
        *state.controller.borrow_mut() = Some(controller);
        self.set_transform_backpressure(&state, true);
        let prototype = prototype.unwrap_or_else(|| match kind {
            PairKind::Transform => self.intrinsics.streams.transform.clone(),
            PairKind::Encoder => self.intrinsics.streams.encoder.clone(),
            PairKind::Decoder(_) => self.intrinsics.streams.decoder.clone(),
            PairKind::Compression => self.intrinsics.streams.compression.clone(),
        });
        let pair = Pair {
            readable: readable_value,
            writable: writable_value,
            kind,
        };
        let object = self.stream_object(&prototype, StreamHandle::Pair(Rc::new(pair)));
        (Value::Object(object), state)
    }

    fn set_transform_backpressure(&mut self, state: &Rc<TransformState>, backpressure: bool) {
        let previous = state.backpressure_change.borrow_mut().take();
        if let Some(previous) = previous {
            self.settle_promise(&previous, PromiseOutcome::Fulfilled(Value::Undefined));
        }
        *state.backpressure_change.borrow_mut() = Some(Self::pending_promise());
        state.backpressure.set(backpressure);
    }

    fn transform_write(
        &mut self,
        state: &Rc<TransformState>,
        chunk: Value,
    ) -> Result<Value, ScriptError> {
        if state.backpressure.get() {
            let change = state
                .backpressure_change
                .borrow()
                .clone()
                .unwrap_or_else(|| self.fulfilled_promise(Value::Undefined));
            let step = self.stream_step(StreamFunction::TransformAfterBackpressure(
                state.clone(),
                chunk,
            ));
            return self.promise_then(change, Some(&step), None);
        }
        self.perform_transform(state, chunk)
    }

    fn perform_transform(
        &mut self,
        state: &Rc<TransformState>,
        chunk: Value,
    ) -> Result<Value, ScriptError> {
        let controller = state
            .controller
            .borrow()
            .clone()
            .map_or(Value::Undefined, Value::Object);
        let outcome = match &state.transformer {
            Transformer::Script {
                object,
                transform: Some(transform),
                ..
            } => self.call_function(transform.clone(), object.clone(), vec![chunk, controller]),
            Transformer::Script { .. } => self
                .transform_enqueue(state, chunk)
                .map(|_| Value::Undefined),
            Transformer::Encode(pending) => {
                let text = self.coerce_string(chunk)?;
                let mut units: Vec<u16> = pending.borrow_mut().take().into_iter().collect();
                units.extend(text.encode_utf16());
                if let Some(&last) = units.last()
                    && (0xd800..0xdc00).contains(&last)
                {
                    *pending.borrow_mut() = units.pop();
                }
                if units.is_empty() {
                    return Ok(Value::Undefined);
                }
                let bytes = String::from_utf16_lossy(&units).into_bytes();
                let length = bytes.len();
                let array = typed::new_typed(
                    typed::ElementKind::Uint8,
                    Rc::new(RefCell::new(bytes.into())),
                    0,
                    length,
                );
                self.transform_enqueue(state, array)
                    .map(|_| Value::Undefined)
            }
            Transformer::Decode(decoder) => {
                let decode = self.get_property(decoder, "decode")?;
                let options = plain_object([("stream".to_string(), Value::Bool(true))]);
                let text = self.call_function(decode, decoder.clone(), vec![chunk, options])?;
                if matches!(&text, Value::String(text) if !text.is_empty()) {
                    self.transform_enqueue(state, text)?;
                }
                Ok(Value::Undefined)
            }
            Transformer::Compress { input, .. } => {
                let bytes = self.buffer_source_bytes(&chunk)?;
                input.borrow_mut().extend(bytes);
                Ok(Value::Undefined)
            }
        };
        match outcome {
            Ok(value) => {
                let promise = self.promise_resolve(value)?;
                let rejected = self.stream_step(StreamFunction::TransformFailed(state.clone()));
                self.promise_then(promise, None, Some(&rejected))
            }
            Err(error) => {
                let reason = self.take_thrown(error);
                self.transform_error(state, reason.clone());
                Err(self.throw_value(reason))
            }
        }
    }

    fn transform_close(&mut self, state: &Rc<TransformState>) -> Result<Value, ScriptError> {
        let controller = state
            .controller
            .borrow()
            .clone()
            .map_or(Value::Undefined, Value::Object);
        let outcome = match &state.transformer {
            Transformer::Script {
                object,
                flush: Some(flush),
                ..
            } => self.call_function(flush.clone(), object.clone(), vec![controller]),
            Transformer::Script { .. } | Transformer::Encode(_) => Ok(Value::Undefined),
            Transformer::Decode(decoder) => {
                let decode = self.get_property(decoder, "decode")?;
                let text = self.call_function(decode, decoder.clone(), Vec::new())?;
                if matches!(&text, Value::String(text) if !text.is_empty()) {
                    self.transform_enqueue(state, text)?;
                }
                Ok(Value::Undefined)
            }
            Transformer::Compress {
                format,
                decompress,
                input,
            } => {
                let input = std::mem::take(&mut *input.borrow_mut());
                let coded = match (format, decompress) {
                    (Compression::Gzip, false) => Some(crate::deflate::gzip(&input)),
                    (Compression::Deflate, false) => Some(crate::deflate::zlib_compress(&input)),
                    (Compression::DeflateRaw, false) => {
                        let zlib = crate::deflate::zlib_compress(&input);
                        zlib.get(2..zlib.len().saturating_sub(4))
                            .map(<[u8]>::to_vec)
                    }
                    (Compression::Gzip, true) => gunzip(&input),
                    (Compression::Deflate, true) => {
                        crate::inflate::zlib_decompress(&input, MAX_INFLATED_BYTES).ok()
                    }
                    (Compression::DeflateRaw, true) => {
                        crate::inflate::inflate(&input, MAX_INFLATED_BYTES)
                            .ok()
                            .map(|(bytes, _)| bytes)
                    }
                };
                match coded {
                    Some(bytes) => {
                        if !bytes.is_empty() {
                            let length = bytes.len();
                            let array = typed::new_typed(
                                typed::ElementKind::Uint8,
                                Rc::new(RefCell::new(bytes.into())),
                                0,
                                length,
                            );
                            self.transform_enqueue(state, array)?;
                        }
                        Ok(Value::Undefined)
                    }
                    None => Err(self.type_error("The compressed data was not valid")),
                }
            }
        };
        match outcome {
            Ok(value) => {
                let promise = self.promise_resolve(value)?;
                let fulfilled = self.stream_step(StreamFunction::Flushed(state.clone(), true));
                let rejected = self.stream_step(StreamFunction::Flushed(state.clone(), false));
                self.promise_then(promise, Some(&fulfilled), Some(&rejected))
            }
            Err(error) => {
                let reason = self.take_thrown(error);
                self.transform_error(state, reason.clone());
                Err(self.throw_value(reason))
            }
        }
    }

    fn transform_enqueue(
        &mut self,
        state: &Rc<TransformState>,
        chunk: Value,
    ) -> Result<(), ScriptError> {
        let readable = state.readable.borrow().clone().expect("made with one");
        if readable.borrow().close_requested || !matches!(readable.borrow().state, State::Readable)
        {
            return Err(self.type_error("Readable side is not in a state that permits enqueue"));
        }
        if let Err(error) = self.enqueue_readable(&readable, chunk) {
            let reason = self.take_thrown(error);
            self.transform_error_writable(state, reason.clone());
            return Err(self.throw_value(reason));
        }
        let full = Self::desired_size(&readable.borrow()).is_some_and(|size| size <= 0.0);
        if full && !state.backpressure.get() {
            self.set_transform_backpressure(state, true);
        }
        Ok(())
    }

    fn transform_error(&mut self, state: &Rc<TransformState>, reason: Value) {
        let readable = state.readable.borrow().clone();
        if let Some(readable) = readable {
            self.error_readable(&readable, reason.clone());
        }
        self.transform_error_writable(state, reason);
    }

    fn transform_error_writable(&mut self, state: &Rc<TransformState>, reason: Value) {
        let writable = state.writable.borrow().clone();
        if let Some(writable) = writable {
            self.controller_error(&writable, reason);
        }
        if state.backpressure.get() {
            self.set_transform_backpressure(state, false);
        }
    }

    fn transformer_cancel(
        &mut self,
        state: &Rc<TransformState>,
        reason: Value,
    ) -> Result<Value, ScriptError> {
        match &state.transformer {
            Transformer::Script {
                object,
                cancel: Some(cancel),
                ..
            } => self.call_function(cancel.clone(), object.clone(), vec![reason]),
            _ => Ok(Value::Undefined),
        }
    }

    // ---- pipes ----

    fn pipe_to(
        &mut self,
        source: &Rc<RefCell<Readable>>,
        dest: &Rc<RefCell<Writable>>,
        options: Value,
    ) -> Result<PromiseHandle, ScriptError> {
        let flag = |runtime: &mut Self, name: &str| -> Result<bool, ScriptError> {
            if is_primitive(&options) {
                return Ok(false);
            }
            Ok(runtime.get_property(&options, name)?.truthy())
        };
        let prevent_close = flag(self, "preventClose")?;
        let prevent_abort = flag(self, "preventAbort")?;
        let prevent_cancel = flag(self, "preventCancel")?;
        let signal = if is_primitive(&options) {
            Value::Undefined
        } else {
            self.get_property(&options, "signal")?
        };
        let signal = match signal {
            Value::Undefined => None,
            Value::AbortSignal(signal) => Some(signal),
            _ => {
                return Err(self.type_error(
                    "Failed to execute 'pipeTo': member signal is not of type AbortSignal.",
                ));
            }
        };
        if source.borrow().reader.is_some() {
            return Ok(self.rejected_promise("Cannot pipe a locked ReadableStream"));
        }
        if dest.borrow().writer.is_some() {
            return Ok(self.rejected_promise("Cannot pipe to a locked WritableStream"));
        }
        let reader = match self.acquire_reader(source)? {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Stream(StreamHandle::Reader(reader)) => reader.clone(),
                _ => unreachable!("acquire_reader makes a reader"),
            },
            _ => unreachable!("acquire_reader makes a reader"),
        };
        let writer = match self.acquire_writer(dest)? {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Stream(StreamHandle::Writer(writer)) => writer.clone(),
                _ => unreachable!("acquire_writer makes a writer"),
            },
            _ => unreachable!("acquire_writer makes a writer"),
        };
        let pipe = Rc::new(Pipe {
            reader,
            writer,
            source: source.clone(),
            dest: dest.clone(),
            prevent_close,
            prevent_abort,
            prevent_cancel,
            result: Self::pending_promise(),
            done: Cell::new(false),
        });
        if let Some(signal) = signal {
            if signal.borrow().aborted {
                let reason = signal.borrow().reason.clone().unwrap_or(Value::Undefined);
                self.pipe_aborted(&pipe, reason)?;
                return Ok(pipe.result.clone());
            }
            let listener =
                self.stream_step(StreamFunction::PipeSignal(pipe.clone(), signal.clone()));
            signal.borrow_mut().listeners.push(listener);
        }
        self.pipe_step(&pipe)?;
        Ok(pipe.result.clone())
    }

    fn pipe_step(&mut self, pipe: &Rc<Pipe>) -> Result<(), ScriptError> {
        if pipe.done.get() {
            return Ok(());
        }
        if let WriteState::Errored(reason) | WriteState::Erroring(reason) =
            pipe.dest.borrow().state.clone()
        {
            return self.pipe_dest_failed(pipe, reason);
        }
        if let State::Errored(reason) = pipe.source.borrow().state.clone() {
            return self.pipe_source_failed(pipe, reason);
        }
        if matches!(pipe.dest.borrow().state, WriteState::Closed)
            || pipe.dest.borrow().close_requested
        {
            let error = self.make_error("TypeError", "the destination stream closed");
            if !pipe.prevent_cancel {
                let promise = self.cancel_readable(&pipe.source, error.clone())?;
                promise.borrow_mut().handled = true;
            }
            return self.pipe_finish(pipe, Some(error));
        }
        let ready = pipe.writer.borrow().ready.clone();
        let fulfilled = self.stream_step(StreamFunction::PipeReady(pipe.clone(), true));
        let rejected = self.stream_step(StreamFunction::PipeReady(pipe.clone(), false));
        self.promise_then(ready, Some(&fulfilled), Some(&rejected))?;
        Ok(())
    }

    fn pipe_ready(
        &mut self,
        pipe: &Rc<Pipe>,
        fulfilled: bool,
        reason: Value,
    ) -> Result<(), ScriptError> {
        if pipe.done.get() {
            return Ok(());
        }
        if !fulfilled {
            return self.pipe_dest_failed(pipe, reason);
        }
        let read = self.read_from(&pipe.reader)?;
        let fulfilled = self.stream_step(StreamFunction::PipeRead(pipe.clone(), true));
        let rejected = self.stream_step(StreamFunction::PipeRead(pipe.clone(), false));
        self.promise_then(read, Some(&fulfilled), Some(&rejected))?;
        Ok(())
    }

    fn pipe_read(
        &mut self,
        pipe: &Rc<Pipe>,
        fulfilled: bool,
        result: Value,
    ) -> Result<(), ScriptError> {
        if pipe.done.get() {
            return Ok(());
        }
        if !fulfilled {
            return self.pipe_source_failed(pipe, result);
        }
        if self.get_property(&result, "done")?.truthy() {
            if pipe.prevent_close {
                return self.pipe_finish(pipe, None);
            }
            let closed = self.close_writable(&pipe.dest)?;
            let fulfilled = self.stream_step(StreamFunction::PipeClosed(pipe.clone(), true));
            let rejected = self.stream_step(StreamFunction::PipeClosed(pipe.clone(), false));
            self.promise_then(closed, Some(&fulfilled), Some(&rejected))?;
            return Ok(());
        }
        let chunk = self.get_property(&result, "value")?;
        let written = self.write_chunk(&pipe.dest, chunk)?;
        written.borrow_mut().handled = true;
        self.pipe_step(pipe)
    }

    fn pipe_source_failed(&mut self, pipe: &Rc<Pipe>, reason: Value) -> Result<(), ScriptError> {
        if !pipe.prevent_abort {
            let promise = self.abort_writable(&pipe.dest, reason.clone())?;
            promise.borrow_mut().handled = true;
        }
        self.pipe_finish(pipe, Some(reason))
    }

    fn pipe_dest_failed(&mut self, pipe: &Rc<Pipe>, reason: Value) -> Result<(), ScriptError> {
        if !pipe.prevent_cancel {
            let promise = self.cancel_readable(&pipe.source, reason.clone())?;
            promise.borrow_mut().handled = true;
        }
        self.pipe_finish(pipe, Some(reason))
    }

    fn pipe_aborted(&mut self, pipe: &Rc<Pipe>, reason: Value) -> Result<(), ScriptError> {
        if pipe.done.get() {
            return Ok(());
        }
        if !pipe.prevent_abort {
            let promise = self.abort_writable(&pipe.dest, reason.clone())?;
            promise.borrow_mut().handled = true;
        }
        if !pipe.prevent_cancel {
            let promise = self.cancel_readable(&pipe.source, reason.clone())?;
            promise.borrow_mut().handled = true;
        }
        self.pipe_finish(pipe, Some(reason))
    }

    fn pipe_finish(&mut self, pipe: &Rc<Pipe>, error: Option<Value>) -> Result<(), ScriptError> {
        if pipe.done.replace(true) {
            return Ok(());
        }
        self.release_writer(&pipe.writer);
        self.release_reader(&pipe.reader);
        let outcome = match error {
            Some(reason) => PromiseOutcome::Rejected(reason),
            None => PromiseOutcome::Fulfilled(Value::Undefined),
        };
        self.settle_promise(&pipe.result, outcome);
        Ok(())
    }

    // ---- the members of the writable side, transforms, pairs and strategies ----

    pub(super) fn call_writable_stream(
        &mut self,
        function: StreamFunction,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use StreamFunction as F;
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        let rejecting = |runtime: &mut Self, error: ScriptError| -> Value {
            let reason = runtime.take_thrown(error);
            Value::Promise(runtime.rejected_promise_value(reason))
        };
        match function {
            F::WritableLocked => {
                let stream = self.writable_of(&this)?;
                let locked = stream.borrow().writer.is_some();
                Ok(Value::Bool(locked))
            }
            F::WritableAbort | F::WritableClose => {
                let stream = match self.writable_of(&this) {
                    Ok(stream) => stream,
                    Err(error) => return Ok(rejecting(self, error)),
                };
                if stream.borrow().writer.is_some() {
                    return Ok(Value::Promise(self.rejected_promise(
                        "Cannot abort or close a locked WritableStream",
                    )));
                }
                let promise = if matches!(function, F::WritableAbort) {
                    self.abort_writable(&stream, first)?
                } else {
                    self.close_writable(&stream)?
                };
                Ok(Value::Promise(promise))
            }
            F::GetWriter => {
                let stream = self.writable_of(&this)?;
                self.acquire_writer(&stream)
            }
            F::WritableError | F::WritableSignal => {
                let Some(StreamHandle::WritableController(stream)) = Self::handle_of(&this) else {
                    return Err(self
                        .type_error("Illegal invocation: not a WritableStreamDefaultController"));
                };
                if matches!(function, F::WritableSignal) {
                    return Ok(Value::AbortSignal(stream.borrow().signal.clone()));
                }
                self.controller_error(&stream, first);
                Ok(Value::Undefined)
            }
            F::WriterWrite | F::WriterClose | F::WriterAbort | F::WriterReady | F::WriterClosed => {
                let Some(writer) = self.writer_of(&this) else {
                    return Ok(Value::Promise(self.rejected_promise(
                        "Illegal invocation: not a WritableStreamDefaultWriter",
                    )));
                };
                match function {
                    F::WriterReady => return Ok(Value::Promise(writer.borrow().ready.clone())),
                    F::WriterClosed => return Ok(Value::Promise(writer.borrow().closed.clone())),
                    _ => {}
                }
                let Some(stream) = writer.borrow().stream.clone() else {
                    return Ok(Value::Promise(
                        self.rejected_promise("This writer has been released"),
                    ));
                };
                let promise = match function {
                    F::WriterWrite => self.write_chunk(&stream, first)?,
                    F::WriterClose => self.close_writable(&stream)?,
                    _ => self.abort_writable(&stream, first)?,
                };
                Ok(Value::Promise(promise))
            }
            F::WriterDesiredSize | F::WriterRelease => {
                let Some(writer) = self.writer_of(&this) else {
                    return Err(
                        self.type_error("Illegal invocation: not a WritableStreamDefaultWriter")
                    );
                };
                if matches!(function, F::WriterRelease) {
                    self.release_writer(&writer);
                    return Ok(Value::Undefined);
                }
                let Some(stream) = writer.borrow().stream.clone() else {
                    return Err(self.type_error("This writer has been released"));
                };
                let size = Self::writable_desired_size(&stream.borrow());
                Ok(size.map_or(Value::Null, Value::Number))
            }
            F::PipeTo | F::PipeThrough => {
                let source = match self.readable_of(&this) {
                    Ok(source) => source,
                    Err(error) if matches!(function, F::PipeTo) => {
                        return Ok(rejecting(self, error));
                    }
                    Err(error) => return Err(error),
                };
                if matches!(function, F::PipeTo) {
                    let dest = match self.writable_of(&first) {
                        Ok(dest) => dest,
                        Err(error) => return Ok(rejecting(self, error)),
                    };
                    let options = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                    return Ok(Value::Promise(self.pipe_to(&source, &dest, options)?));
                }
                if is_primitive(&first) {
                    return Err(self.type_error(
                        "Failed to execute 'pipeThrough': the transform is not an object",
                    ));
                }
                let writable = self.get_property(&first, "writable")?;
                let readable = self.get_property(&first, "readable")?;
                let dest = self.writable_of(&writable)?;
                self.readable_of(&readable)?;
                if source.borrow().reader.is_some() {
                    return Err(self.type_error("Cannot pipe a locked ReadableStream"));
                }
                if dest.borrow().writer.is_some() {
                    return Err(self.type_error("Cannot pipe to a locked WritableStream"));
                }
                let options = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                let piped = self.pipe_to(&source, &dest, options)?;
                piped.borrow_mut().handled = true;
                Ok(readable)
            }
            F::PairReadable
            | F::PairWritable
            | F::PairEncoding
            | F::PairFatal
            | F::PairIgnoreBom => {
                let Some(StreamHandle::Pair(pair)) = Self::handle_of(&this) else {
                    return Err(self.type_error("Illegal invocation"));
                };
                match function {
                    F::PairReadable => Ok(pair.readable.clone()),
                    F::PairWritable => Ok(pair.writable.clone()),
                    _ => match &pair.kind {
                        PairKind::Encoder => Ok(Value::String("utf-8".into())),
                        PairKind::Decoder(decoder) => {
                            let key = match function {
                                F::PairEncoding => "encoding",
                                F::PairFatal => "fatal",
                                _ => "ignoreBOM",
                            };
                            self.get_property(decoder, key)
                        }
                        _ => Ok(Value::Undefined),
                    },
                }
            }
            F::TransformDesiredSize
            | F::TransformEnqueue
            | F::TransformError
            | F::TransformTerminate => {
                let Some(StreamHandle::TransformController(state)) = Self::handle_of(&this) else {
                    return Err(self
                        .type_error("Illegal invocation: not a TransformStreamDefaultController"));
                };
                match function {
                    F::TransformDesiredSize => {
                        let readable = state.readable.borrow().clone().expect("made with one");
                        let size = Self::desired_size(&readable.borrow());
                        Ok(size.map_or(Value::Null, Value::Number))
                    }
                    F::TransformEnqueue => {
                        self.transform_enqueue(&state, first)?;
                        Ok(Value::Undefined)
                    }
                    F::TransformError => {
                        self.transform_error(&state, first);
                        Ok(Value::Undefined)
                    }
                    _ => {
                        let readable = state.readable.borrow().clone().expect("made with one");
                        if matches!(readable.borrow().state, State::Readable)
                            && !readable.borrow().close_requested
                        {
                            self.close_requested(&readable);
                        }
                        let error = self.make_error("TypeError", "TransformStream terminated");
                        self.transform_error_writable(&state, error);
                        Ok(Value::Undefined)
                    }
                }
            }
            F::StrategyMark => match Self::handle_of(&this) {
                Some(StreamHandle::Strategy(mark)) => Ok(Value::Number(mark)),
                _ => Err(self.type_error("Illegal invocation")),
            },
            F::StrategySize => Ok(self.native_function(native(F::CountOf), "size", 0)),
            F::ByteSize => Ok(self.native_function(native(F::ByteLengthOf), "size", 1)),
            F::CountOf => Ok(Value::Number(1.0)),
            F::ByteLengthOf => self.get_property(&first, "byteLength"),
            F::WriterStarted(stream, fulfilled) => {
                if fulfilled {
                    stream.borrow_mut().started = true;
                    self.advance_writable(&stream)?;
                } else {
                    stream.borrow_mut().started = true;
                    self.controller_error(&stream, first);
                    if matches!(stream.borrow().state, WriteState::Erroring(_)) {
                        self.finish_erroring(&stream)?;
                    }
                }
                Ok(Value::Undefined)
            }
            F::Wrote(stream, closing, fulfilled) => {
                self.wrote(&stream, closing, fulfilled, first)?;
                Ok(Value::Undefined)
            }
            F::TransformAfterBackpressure(state, chunk) => {
                let writable = state.writable.borrow().clone().expect("made with one");
                if let WriteState::Erroring(reason) | WriteState::Errored(reason) =
                    writable.borrow().state.clone()
                {
                    return Err(self.throw_value(reason));
                }
                self.perform_transform(&state, chunk)
            }
            F::TransformFailed(state) => {
                self.transform_error(&state, first.clone());
                Err(self.throw_value(first))
            }
            F::Flushed(state, fulfilled) => {
                let readable = state.readable.borrow().clone().expect("made with one");
                if fulfilled {
                    if matches!(readable.borrow().state, State::Readable)
                        && !readable.borrow().close_requested
                    {
                        self.close_requested(&readable);
                    }
                    Ok(Value::Undefined)
                } else {
                    self.error_readable(&readable, first.clone());
                    Err(self.throw_value(first))
                }
            }
            F::PipeReady(pipe, fulfilled) => {
                self.pipe_ready(&pipe, fulfilled, first)?;
                Ok(Value::Undefined)
            }
            F::PipeRead(pipe, fulfilled) => {
                self.pipe_read(&pipe, fulfilled, first)?;
                Ok(Value::Undefined)
            }
            F::PipeClosed(pipe, fulfilled) => {
                let error = (!fulfilled).then_some(first);
                self.pipe_finish(&pipe, error)?;
                Ok(Value::Undefined)
            }
            F::Collected(collect, fulfilled) => {
                self.collected(&collect, fulfilled, first)?;
                Ok(Value::Undefined)
            }
            F::PipeSignal(pipe, signal) => {
                let reason = signal.borrow().reason.clone().unwrap_or(Value::Undefined);
                self.pipe_aborted(&pipe, reason)?;
                Ok(Value::Undefined)
            }
            _ => Err(self.type_error("Illegal invocation")),
        }
    }

    pub(super) fn construct_writable_side(
        &mut self,
        function: &StreamFunction,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        use StreamFunction as F;
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        let prototype = match &new_target {
            Value::Object(_) => Some(self.prototype_from_constructor(&new_target)?),
            _ => None,
        };
        match function {
            F::WritableConstructor => {
                self.construct_writable(first, arguments.get(1).cloned(), prototype)
            }
            F::WriterConstructor => {
                let stream = self.writable_of(&first)?;
                self.acquire_writer(&stream)
            }
            F::TransformConstructor => self.construct_transform(
                first,
                arguments.get(1).cloned(),
                arguments.get(2).cloned(),
                prototype,
            ),
            F::EncoderStreamConstructor => {
                let (pair, state) = self.new_transform(
                    Transformer::Encode(RefCell::new(None)),
                    (1.0, None),
                    (0.0, None),
                    prototype,
                    PairKind::Encoder,
                );
                self.start_pair(&state)?;
                Ok(pair)
            }
            F::DecoderStreamConstructor => {
                let decoder_class = self
                    .lookup_global("TextDecoder")
                    .unwrap_or(Value::Undefined);
                let decoder = self.construct(decoder_class, arguments, None)?;
                let (pair, state) = self.new_transform(
                    Transformer::Decode(decoder.clone()),
                    (1.0, None),
                    (0.0, None),
                    prototype,
                    PairKind::Decoder(decoder),
                );
                self.start_pair(&state)?;
                Ok(pair)
            }
            F::CompressionConstructor(decompress) => {
                let format = self.coerce_string(first)?;
                let format = match format.as_str() {
                    "gzip" => Compression::Gzip,
                    "deflate" => Compression::Deflate,
                    "deflate-raw" => Compression::DeflateRaw,
                    _ => {
                        return Err(self.type_error(format!(
                            "Failed to construct: Unsupported compression format: '{format}'"
                        )));
                    }
                };
                let (pair, state) = self.new_transform(
                    Transformer::Compress {
                        format,
                        decompress: *decompress,
                        input: RefCell::new(Vec::new()),
                    },
                    (1.0, None),
                    (0.0, None),
                    prototype,
                    PairKind::Compression,
                );
                self.start_pair(&state)?;
                Ok(pair)
            }
            F::StrategyConstructor(bytes) => {
                if is_primitive(&first) {
                    return Err(
                        self.type_error("Failed to construct: the strategy init is not an object")
                    );
                }
                let mark = self.get_property(&first, "highWaterMark")?;
                if matches!(mark, Value::Undefined) {
                    return Err(self.type_error(
                        "Failed to construct: required member highWaterMark is undefined.",
                    ));
                }
                let mark = self.coerce_number(mark)?;
                let prototype = prototype.unwrap_or_else(|| {
                    if *bytes {
                        self.intrinsics.streams.byte_strategy.clone()
                    } else {
                        self.intrinsics.streams.count_strategy.clone()
                    }
                });
                Ok(Value::Object(
                    self.stream_object(&prototype, StreamHandle::Strategy(mark)),
                ))
            }
            _ => Err(self.type_error("Illegal constructor")),
        }
    }

    // a built-in transform has nothing to start, so both sides start at once
    fn start_pair(&mut self, state: &Rc<TransformState>) -> Result<(), ScriptError> {
        let readable = state.readable.borrow().clone().expect("made with one");
        let writable = state.writable.borrow().clone().expect("made with one");
        readable.borrow_mut().started = true;
        self.start_writable(&writable, Value::Undefined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(source: &str) -> String {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(&format!(
                "(async () => {{ {source} }})().then((value) => {{ document.getElementById('out').textContent = String(value); }}, (error) => {{ document.getElementById('out').textContent = 'threw ' + error; }});"
            ))
            .unwrap_or_else(|error| panic!("{source}: {error:?}"));
        runtime.advance_time(1).expect("timers should run");
        let out = document.borrow().find_element_by_id("out").unwrap();
        document
            .borrow()
            .element_text_content(out)
            .unwrap_or_default()
    }

    #[test]
    fn readable_streams_pull_queue_and_close() {
        assert_eq!(
            run(r#"
                let n = 0;
                const stream = new ReadableStream({
                    start(controller) { controller.enqueue("a"); },
                    pull(controller) { n++; if (n > 2) controller.close(); else controller.enqueue("p" + n); },
                }, { highWaterMark: 0 });
                const seen = [stream.locked];
                for await (const chunk of stream) seen.push(chunk);
                seen.push(stream.locked);
                return seen.join();
            "#),
            "false,a,p1,p2,false"
        );
        assert_eq!(
            run(r#"
                const stream = new ReadableStream({ start(c) { c.enqueue(1); c.enqueue(2); c.close(); } });
                const [left, right] = stream.tee();
                const read = async (s) => { const out = []; for await (const v of s) out.push(v); return out.join(""); };
                return [stream.locked, await read(left), await read(right)].join();
            "#),
            "true,12,12"
        );
        assert_eq!(
            run(r#"
                let reason;
                const stream = new ReadableStream({ cancel(why) { reason = why; } });
                await stream.cancel("stop");
                const reader = new ReadableStream({ start(c) { c.error(new Error("broke")); } }).getReader();
                let caught;
                try { await reader.read(); } catch (error) { caught = error.message; }
                return [reason, caught, Object.prototype.toString.call(stream)].join();
            "#),
            "stop,broke,[object ReadableStream]"
        );
        assert_eq!(
            run(r#"
                async function* letters() { yield "x"; yield "y"; }
                const out = [];
                for await (const v of ReadableStream.from(letters())) out.push(v);
                for await (const v of ReadableStream.from([Promise.resolve(1), 2])) out.push(v);
                const reader = ReadableStream.from([3]).getReader();
                const first = await reader.read();
                const second = await reader.read();
                out.push(first.value, second.done);
                try { reader.releaseLock(); reader.releaseLock(); new ReadableStream().getReader().releaseLock(); } catch (error) { out.push(error.name); }
                return out.join();
            "#),
            "x,y,1,2,3,true"
        );
        assert_eq!(
            run(r#"
                const stream = new ReadableStream({ start(c) { c.enqueue("a"); } }, { highWaterMark: 3, size: () => 2 });
                let controller;
                new ReadableStream({ start(c) { controller = c; } });
                const reader = stream.getReader();
                let locked;
                try { stream.getReader(); } catch (error) { locked = error.name; }
                return [controller.desiredSize, locked, typeof reader.closed.then].join();
            "#),
            "1,TypeError,function"
        );
    }
    #[test]
    fn writable_streams_write_in_order_and_report_backpressure() {
        assert_eq!(
            run(r#"
                const written = [];
                const stream = new WritableStream({
                    write(chunk) { return new Promise((done) => setTimeout(() => { written.push(chunk); done(); }, 0)); },
                    close() { written.push("closed"); },
                }, new CountQueuingStrategy({ highWaterMark: 2 }));
                const writer = stream.getWriter();
                const sizes = [writer.desiredSize];
                writer.write("a");
                writer.write("b");
                sizes.push(writer.desiredSize);
                await writer.ready;
                await writer.close();
                await writer.closed;
                return [written.join(""), sizes.join("/"), stream.locked].join();
            "#),
            "abclosed,2/0,true"
        );
        assert_eq!(
            run(r#"
                let aborted;
                const stream = new WritableStream({ abort(reason) { aborted = reason; } });
                const writer = stream.getWriter();
                await writer.abort("gone");
                let failed;
                try { await writer.write("x"); } catch (error) { failed = error; }
                const broken = new WritableStream({ write() { throw new Error("full"); } }).getWriter();
                let message;
                try { await broken.write(1); } catch (error) { message = error.message; }
                return [aborted, failed, message].join();
            "#),
            "gone,gone,full"
        );
    }

    #[test]
    fn transforms_and_pipes_carry_chunks_through() {
        assert_eq!(
            run(r#"
                const upper = new TransformStream({
                    transform(chunk, controller) { controller.enqueue(chunk.toUpperCase()); },
                    flush(controller) { controller.enqueue("!"); },
                });
                const source = ReadableStream.from(["a", "b"]);
                const out = [];
                for await (const chunk of source.pipeThrough(upper)) out.push(chunk);
                const sink = [];
                await ReadableStream.from([1, 2, 3]).pipeTo(new WritableStream({ write(c) { sink.push(c); } }));
                return out.join("") + "|" + sink.join("");
            "#),
            "AB!|123"
        );
        assert_eq!(
            run(r#"
                const bytes = ReadableStream.from(["héllo ", "wörld"]).pipeThrough(new TextEncoderStream());
                const text = bytes.pipeThrough(new TextDecoderStream());
                let joined = "";
                for await (const chunk of text) joined += chunk;
                const packed = ReadableStream.from([new TextEncoder().encode("zip me zip me zip me")])
                    .pipeThrough(new CompressionStream("gzip"))
                    .pipeThrough(new DecompressionStream("gzip"))
                    .pipeThrough(new TextDecoderStream());
                let unpacked = "";
                for await (const chunk of packed) unpacked += chunk;
                const strategy = new ByteLengthQueuingStrategy({ highWaterMark: 16 });
                return [joined, unpacked, strategy.highWaterMark, strategy.size(new Uint8Array(5)), new TextDecoderStream().encoding].join("|");
            "#),
            "héllo wörld|zip me zip me zip me|16|5|utf-8"
        );
        assert_eq!(
            run(r#"
                const controller = new AbortController();
                const source = new ReadableStream({ pull(c) { c.enqueue(1); } });
                const done = source.pipeTo(new WritableStream(), { signal: controller.signal });
                controller.abort(new Error("stopped"));
                try { await done; return "resolved"; } catch (error) { return error.message; }
            "#),
            "stopped"
        );
    }
    #[test]
    fn responses_read_streams_and_blobs_hand_out_streams() {
        assert_eq!(
            run(r#"
                const encoder = new TextEncoder();
                const source = ReadableStream.from([encoder.encode('{"a":'), encoder.encode('1}')]);
                const parsed = await new Response(source).json();
                const text = await new Response(new Blob(["blob text"]).stream()).text();
                const piped = await new Response(
                    ReadableStream.from(["x", "y"]).pipeThrough(new TextEncoderStream())
                ).arrayBuffer();
                let refused;
                try { await new Response(ReadableStream.from(["not bytes"])).text(); } catch (error) { refused = error.name; }
                return [parsed.a, text, piped.byteLength, refused].join("|");
            "#),
            "1|blob text|2|TypeError"
        );
    }
}
