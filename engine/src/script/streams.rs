// ReadableStream from the streams standard: a queue of chunks fed by an underlying source
// through its controller and drained by a default reader, with cancel, tee, async iteration
// and ReadableStream.from; response bodies are streams of this kind fed from their bytes

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, new_object};
use super::operations::{is_callable, is_primitive};
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

    StreamInterfaces {
        readable,
        controller,
        reader,
        iterator,
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

    fn handle_of(value: &Value) -> Option<StreamHandle> {
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
            _ => Err(self.type_error("Illegal constructor")),
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
                            Rc::new(RefCell::new(chunk)),
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
        }
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
}
