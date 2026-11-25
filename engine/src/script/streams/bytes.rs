// readable byte streams: a ReadableByteStreamController queues bytes rather than chunks and
// fills the views that BYOB readers and auto-allocating default reads bring, through the
// pull-into descriptors of the streams standard

use super::*;

#[derive(Clone, Copy)]
pub(super) enum ViewKind {
    Typed(typed::ElementKind),
    Data,
}

impl ViewKind {
    fn size(self) -> usize {
        match self {
            ViewKind::Typed(kind) => kind.size(),
            ViewKind::Data => 1,
        }
    }
}

// who waits on a pull-into: a default reader's read, a BYOB read, or no one once released
#[derive(Clone, Copy, PartialEq, Eq)]
enum Taker {
    Default,
    Byob,
    Released,
}

struct PullInto {
    buffer: BytesHandle,
    buffer_length: usize,
    offset: usize,
    length: usize,
    filled: usize,
    minimum: usize,
    kind: ViewKind,
    taker: Taker,
}

#[derive(Default)]
pub(super) struct ByteSource {
    pub(super) auto_allocate: Option<usize>,
    chunks: VecDeque<(BytesHandle, usize, usize)>,
    pending: VecDeque<PullInto>,
    request: Option<(ObjectHandle, Rc<RefCell<ByobRequest>>)>,
}

impl ByteSource {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        if let Some((obj, req)) = &self.request {
            tracer.mark_object(obj.clone());
            trace_byob_request(req, tracer);
        }
    }
}

pub(super) fn trace_byob_request(req: &Rc<RefCell<ByobRequest>>, tracer: &mut super::heap::Tracer) {
    if !tracer.visit_rc(req) {
        return;
    }
    let req = req.borrow();
    if let Some(stream) = &req.stream {
        super::trace_readable(stream, tracer);
    }
    if let Some(view) = &req.view {
        tracer.trace_value(view);
    }
}

pub(in crate::script) struct ByobRequest {
    stream: Option<Rc<RefCell<Readable>>>,
    view: Option<Value>,
}

// an ArrayBufferView as the byte stream sees it; a detached view has no length
pub(super) struct View {
    buffer: BytesHandle,
    offset: usize,
    length: usize,
    kind: ViewKind,
}

pub(super) fn view_of(value: &Value) -> Option<View> {
    match value {
        Value::TypedArray(array) => {
            let array = array.borrow();
            let length = typed::typed_range(&array).map_or(0, |(start, end)| end - start);
            Some(View {
                buffer: array.buffer.clone(),
                offset: array.start,
                length,
                kind: ViewKind::Typed(array.kind),
            })
        }
        Value::Object(object) => match &object.borrow().class {
            ObjectClass::DataView(view) => {
                let crate::script::object::DataViewRange {
                    buffer,
                    start,
                    end,
                    tracking,
                } = &**view;
                let range = typed::view_range(&buffer.borrow(), *start, *end, *tracking, 1);
                Some(View {
                    buffer: buffer.clone(),
                    offset: *start,
                    length: range.map_or(0, |(start, end)| end - start),
                    kind: ViewKind::Data,
                })
            }
            _ => None,
        },
        _ => None,
    }
}

// TransferArrayBuffer: the bytes move to a fresh buffer and the old one is detached
fn transfer(buffer: &BytesHandle) -> BytesHandle {
    let bytes = std::mem::take(&mut buffer.borrow_mut().bytes);
    buffer.borrow_mut().detached = true;
    Rc::new(RefCell::new(ByteBuffer {
        bytes,
        max: None,
        detached: false,
    }))
}

fn byte_source(stream: &mut Readable) -> &mut ByteSource {
    stream.bytes.as_mut().expect("a byte stream")
}

fn queue_chunk(stream: &Rc<RefCell<Readable>>, buffer: BytesHandle, offset: usize, length: usize) {
    let mut stream = stream.borrow_mut();
    byte_source(&mut stream)
        .chunks
        .push_back((buffer, offset, length));
    stream.queued += length as f64;
}

fn queue_copy(stream: &Rc<RefCell<Readable>>, buffer: &BytesHandle, offset: usize, length: usize) {
    let copy = buffer
        .borrow()
        .get(offset..offset + length)
        .unwrap_or_default()
        .to_vec();
    queue_chunk(stream, Rc::new(RefCell::new(copy.into())), 0, length);
}

// moves queued bytes into the head pull-into; true once it holds at least its minimum
fn fill_head(stream: &mut Readable) -> bool {
    let queued = stream.queued as usize;
    let source = byte_source(stream);
    let Some(head) = source.pending.front_mut() else {
        return false;
    };
    let most = queued.min(head.length - head.filled);
    let most_filled = head.filled + most;
    let aligned = most_filled - most_filled % head.kind.size();
    let (mut remaining, ready) = if aligned >= head.minimum {
        (aligned - head.filled, true)
    } else {
        (most, false)
    };
    let mut moved = 0;
    while remaining > 0 {
        let Some((buffer, offset, length)) = source.chunks.front_mut() else {
            break;
        };
        let count = remaining.min(*length);
        let Some(bytes) = buffer
            .borrow()
            .get(*offset..*offset + count)
            .map(<[u8]>::to_vec)
        else {
            break;
        };
        let at = head.offset + head.filled;
        match head.buffer.borrow_mut().get_mut(at..at + count) {
            Some(slot) => slot.copy_from_slice(&bytes),
            None => break,
        }
        if count == *length {
            source.chunks.pop_front();
        } else {
            *offset += count;
            *length -= count;
        }
        head.filled += count;
        remaining -= count;
        moved += count;
    }
    stream.queued = (stream.queued - moved as f64).max(0.0);
    ready
}

fn invalidate_request(stream: &Rc<RefCell<Readable>>) {
    let taken = stream
        .borrow_mut()
        .bytes
        .as_mut()
        .and_then(|source| source.request.take());
    if let Some((_, request)) = taken {
        let mut request = request.borrow_mut();
        request.stream = None;
        request.view = None;
    }
}

fn pop_pending(stream: &Rc<RefCell<Readable>>) -> Option<PullInto> {
    byte_source(&mut stream.borrow_mut()).pending.pop_front()
}

impl ScriptRuntime {
    fn make_view(
        &self,
        kind: ViewKind,
        buffer: BytesHandle,
        offset: usize,
        length: usize,
    ) -> Value {
        match kind {
            ViewKind::Typed(kind) => typed::new_typed(kind, buffer, offset, offset + length),
            ViewKind::Data => Value::Object(new_object(JsObject::with_prototype(
                Some(self.intrinsics.data_view_prototype.clone()),
                ObjectClass::DataView(Box::new(crate::script::object::DataViewRange {
                    buffer,
                    start: offset,
                    end: offset + length,
                    tracking: false,
                })),
            ))),
        }
    }

    // [EnforceRange] unsigned long long
    fn enforce_unsigned(&mut self, value: Value, label: &str) -> Result<usize, ScriptError> {
        let number = self.coerce_number(value)?;
        if !number.is_finite() || number.trunc() < 0.0 || number.trunc() > 9007199254740991.0 {
            return Err(self.type_error(format!(
                "{label} is outside the accepted range of unsigned long long"
            )));
        }
        Ok(number.trunc() as usize)
    }

    // the underlying source's autoAllocateChunkSize, when it gives one
    pub(super) fn auto_allocate_size(
        &mut self,
        source: &Value,
    ) -> Result<Option<usize>, ScriptError> {
        if matches!(source, Value::Undefined) {
            return Ok(None);
        }
        match self.get_property(source, "autoAllocateChunkSize")? {
            Value::Undefined => Ok(None),
            value => Ok(Some(self.enforce_unsigned(value, "autoAllocateChunkSize")?)),
        }
    }

    fn commit_pull_into(&mut self, stream: &Rc<RefCell<Readable>>, descriptor: PullInto) {
        let done = matches!(stream.borrow().state, State::Closed);
        let length = descriptor.filled / descriptor.kind.size() * descriptor.kind.size();
        let view = self.make_view(
            descriptor.kind,
            transfer(&descriptor.buffer),
            descriptor.offset,
            length,
        );
        let request = stream
            .borrow()
            .reader
            .as_ref()
            .and_then(|reader| reader.borrow_mut().requests.pop_front());
        let Some(request) = request else {
            return;
        };
        let result = if done && descriptor.taker == Taker::Default {
            read_result(Value::Undefined, true)
        } else {
            read_result(view, done)
        };
        self.settle_promise(&request, PromiseOutcome::Fulfilled(result));
    }

    fn fill_pending_from_queue(&mut self, stream: &Rc<RefCell<Readable>>) {
        loop {
            let filled = {
                let mut stream = stream.borrow_mut();
                if stream.queued == 0.0 || !fill_head(&mut stream) {
                    break;
                }
                byte_source(&mut stream).pending.pop_front()
            };
            match filled {
                Some(descriptor) => self.commit_pull_into(stream, descriptor),
                None => break,
            }
        }
    }

    fn handle_queue_drain(&mut self, stream: &Rc<RefCell<Readable>>) -> Result<(), ScriptError> {
        let drained = stream.borrow().queued == 0.0 && stream.borrow().close_requested;
        if drained {
            self.close_readable(stream);
            Ok(())
        } else {
            self.call_pull_if_needed(stream)
        }
    }

    fn fill_read_from_queue(
        &mut self,
        stream: &Rc<RefCell<Readable>>,
        request: &PromiseHandle,
    ) -> Result<(), ScriptError> {
        let (buffer, offset, length) = {
            let mut stream = stream.borrow_mut();
            let chunk = byte_source(&mut stream)
                .chunks
                .pop_front()
                .expect("a queued chunk");
            stream.queued = (stream.queued - chunk.2 as f64).max(0.0);
            chunk
        };
        self.handle_queue_drain(stream)?;
        let view = typed::new_typed(typed::ElementKind::Uint8, buffer, offset, offset + length);
        self.settle_promise(request, PromiseOutcome::Fulfilled(read_result(view, false)));
        Ok(())
    }

    // a pull-into no reader waits on any more hands what it holds back to the queue
    fn queue_released_head(&mut self, stream: &Rc<RefCell<Readable>>) {
        if let Some(head) = pop_pending(stream)
            && head.filled > 0
        {
            queue_copy(stream, &head.buffer, head.offset, head.filled);
        }
    }

    pub(super) fn byte_enqueue(
        &mut self,
        stream: &Rc<RefCell<Readable>>,
        view: View,
    ) -> Result<(), ScriptError> {
        if view.buffer.borrow().detached {
            return Err(self.type_error("The chunk's buffer is detached"));
        }
        let transferred = transfer(&view.buffer);
        let head = byte_source(&mut stream.borrow_mut())
            .pending
            .front()
            .map(|head| (head.buffer.borrow().detached, head.taker));
        if let Some((detached, taker)) = head {
            if detached {
                return Err(self.type_error("The BYOB request's buffer has been detached"));
            }
            invalidate_request(stream);
            {
                let mut stream = stream.borrow_mut();
                let head = byte_source(&mut stream)
                    .pending
                    .front_mut()
                    .expect("a pending read");
                head.buffer = transfer(&head.buffer);
            }
            if taker == Taker::Released {
                self.queue_released_head(stream);
            }
        }
        let reader = stream.borrow().reader.clone();
        match reader {
            Some(reader) if !reader.borrow().byob => {
                loop {
                    if stream.borrow().queued == 0.0 {
                        break;
                    }
                    let Some(request) = reader.borrow_mut().requests.pop_front() else {
                        break;
                    };
                    self.fill_read_from_queue(stream, &request)?;
                }
                let request = reader.borrow_mut().requests.pop_front();
                match request {
                    None => queue_chunk(stream, transferred, view.offset, view.length),
                    Some(request) => {
                        pop_pending(stream);
                        let chunk = typed::new_typed(
                            typed::ElementKind::Uint8,
                            transferred,
                            view.offset,
                            view.offset + view.length,
                        );
                        self.settle_promise(
                            &request,
                            PromiseOutcome::Fulfilled(read_result(chunk, false)),
                        );
                    }
                }
            }
            Some(_) => {
                queue_chunk(stream, transferred, view.offset, view.length);
                self.fill_pending_from_queue(stream);
            }
            None => queue_chunk(stream, transferred, view.offset, view.length),
        }
        self.call_pull_if_needed(stream)
    }

    // closes a byte stream, or marks it to close once its queue drains; a pull-into left
    // holding part of an element errors it, and that error comes back
    pub(super) fn byte_close(&mut self, stream: &Rc<RefCell<Readable>>) -> Option<Value> {
        stream.borrow_mut().close_requested = true;
        if stream.borrow().queued > 0.0 {
            return None;
        }
        let partial = byte_source(&mut stream.borrow_mut())
            .pending
            .front()
            .is_some_and(|head| head.filled % head.kind.size() != 0);
        if partial {
            let error = self.make_error(
                "TypeError",
                "Insufficient bytes to fill elements in the given buffer",
            );
            self.error_readable(stream, error.clone());
            return Some(error);
        }
        self.close_readable(stream);
        None
    }

    // a default read of a byte stream: queued bytes first, else an allocated pull-into
    pub(super) fn byte_read(
        &mut self,
        stream: &Rc<RefCell<Readable>>,
        reader: &Rc<RefCell<Reader>>,
    ) -> Result<PromiseHandle, ScriptError> {
        let request = Self::pending_promise();
        if stream.borrow().queued > 0.0 {
            self.fill_read_from_queue(stream, &request)?;
            return Ok(request);
        }
        let auto_allocate = byte_source(&mut stream.borrow_mut()).auto_allocate;
        if let Some(size) = auto_allocate {
            if size > MAX_TYPED_ARRAY_BYTES {
                let error = self.make_error("RangeError", "Array buffer allocation failed");
                return Ok(self.rejected_promise_value(error));
            }
            byte_source(&mut stream.borrow_mut())
                .pending
                .push_back(PullInto {
                    buffer: Rc::new(RefCell::new(vec![0; size].into())),
                    buffer_length: size,
                    offset: 0,
                    length: size,
                    filled: 0,
                    minimum: 1,
                    kind: ViewKind::Typed(typed::ElementKind::Uint8),
                    taker: Taker::Default,
                });
        }
        reader.borrow_mut().requests.push_back(request.clone());
        self.call_pull_if_needed(stream)?;
        Ok(request)
    }

    fn byob_read(
        &mut self,
        reader: &Rc<RefCell<Reader>>,
        view: Value,
        options: Value,
    ) -> Result<Value, ScriptError> {
        match self.byob_read_checked(reader, view, options) {
            Ok(promise) => Ok(Value::Promise(promise)),
            Err(error) => {
                let reason = self.take_thrown(error);
                Ok(Value::Promise(self.rejected_promise_value(reason)))
            }
        }
    }

    fn byob_read_checked(
        &mut self,
        reader: &Rc<RefCell<Reader>>,
        view: Value,
        options: Value,
    ) -> Result<PromiseHandle, ScriptError> {
        let failed = "Failed to execute 'read' on 'ReadableStreamBYOBReader'";
        let Some(view) = view_of(&view) else {
            return Err(self.type_error(format!(
                "{failed}: parameter 1 is not of type 'ArrayBufferView'."
            )));
        };
        let minimum = match options {
            Value::Undefined | Value::Null => 1,
            options if is_primitive(&options) => {
                return Err(self.type_error(format!(
                    "{failed}: The provided value is not of type 'ReadableStreamBYOBReaderReadOptions'."
                )));
            }
            options => match self.get_property(&options, "min")? {
                Value::Undefined => 1,
                value => self.enforce_unsigned(value, "min")?,
            },
        };
        if view.length == 0 {
            return Err(self.type_error(format!(
                "{failed}: This readable stream reader cannot be used to read as the view has byte length equal to 0"
            )));
        }
        if view.buffer.borrow().is_empty() || view.buffer.borrow().detached {
            return Err(self.type_error(format!(
                "{failed}: This readable stream reader cannot be used to read as the viewed array buffer is detached or has byte length equal to 0"
            )));
        }
        if minimum == 0 {
            return Err(self.type_error(format!("{failed}: min must be greater than 0")));
        }
        if minimum > view.length / view.kind.size() {
            return Err(self.range_error(format!(
                "{failed}: min must be less than or equal to the view's length"
            )));
        }
        let Some(stream) = reader.borrow().stream.clone() else {
            return Err(self.type_error(format!(
                "{failed}: This readable stream reader has been released and cannot be used to read from its previous owner stream"
            )));
        };
        stream.borrow_mut().disturbed = true;
        if let State::Errored(reason) = stream.borrow().state.clone() {
            return Err(self.throw_value(reason));
        }
        self.pull_into(&stream, reader, view, minimum)
    }

    fn pull_into(
        &mut self,
        stream: &Rc<RefCell<Readable>>,
        reader: &Rc<RefCell<Reader>>,
        view: View,
        minimum: usize,
    ) -> Result<PromiseHandle, ScriptError> {
        let buffer = transfer(&view.buffer);
        let buffer_length = buffer.borrow().len();
        let descriptor = PullInto {
            buffer,
            buffer_length,
            offset: view.offset,
            length: view.length,
            filled: 0,
            minimum: minimum * view.kind.size(),
            kind: view.kind,
            taker: Taker::Byob,
        };
        let request = Self::pending_promise();
        let waiting = !byte_source(&mut stream.borrow_mut()).pending.is_empty();
        if waiting {
            byte_source(&mut stream.borrow_mut())
                .pending
                .push_back(descriptor);
            reader.borrow_mut().requests.push_back(request.clone());
            return Ok(request);
        }
        if matches!(stream.borrow().state, State::Closed) {
            let empty = self.make_view(descriptor.kind, descriptor.buffer, descriptor.offset, 0);
            self.settle_promise(
                &request,
                PromiseOutcome::Fulfilled(read_result(empty, true)),
            );
            return Ok(request);
        }
        byte_source(&mut stream.borrow_mut())
            .pending
            .push_back(descriptor);
        if stream.borrow().queued > 0.0 {
            let ready = fill_head(&mut stream.borrow_mut());
            if ready {
                let descriptor = pop_pending(stream).expect("the read just queued");
                let length = descriptor.filled / descriptor.kind.size() * descriptor.kind.size();
                let filled = self.make_view(
                    descriptor.kind,
                    transfer(&descriptor.buffer),
                    descriptor.offset,
                    length,
                );
                self.handle_queue_drain(stream)?;
                self.settle_promise(
                    &request,
                    PromiseOutcome::Fulfilled(read_result(filled, false)),
                );
                return Ok(request);
            }
            if stream.borrow().close_requested {
                let error = self.make_error(
                    "TypeError",
                    "Insufficient bytes to fill elements in the given buffer",
                );
                self.error_readable(stream, error.clone());
                return Err(self.throw_value(error));
            }
        }
        reader.borrow_mut().requests.push_back(request.clone());
        self.call_pull_if_needed(stream)?;
        Ok(request)
    }

    fn byob_request(&self, stream: &Rc<RefCell<Readable>>) -> Value {
        let mut borrowed = stream.borrow_mut();
        let source = byte_source(&mut borrowed);
        if let Some((object, _)) = &source.request {
            return Value::Object(object.clone());
        }
        let Some(head) = source.pending.front() else {
            return Value::Null;
        };
        let view = typed::new_typed(
            typed::ElementKind::Uint8,
            head.buffer.clone(),
            head.offset + head.filled,
            head.offset + head.length,
        );
        let request = Rc::new(RefCell::new(ByobRequest {
            stream: Some(stream.clone()),
            view: Some(view),
        }));
        let prototype = self.intrinsics.streams.byob_request.clone();
        let object = self.stream_object(&prototype, StreamHandle::ByobRequest(request.clone()));
        source.request = Some((object.clone(), request));
        Value::Object(object)
    }

    fn request_stream(
        &mut self,
        request: &Rc<RefCell<ByobRequest>>,
    ) -> Result<Rc<RefCell<Readable>>, ScriptError> {
        let stream = request.borrow().stream.clone();
        let view = request.borrow().view.clone();
        let (Some(stream), Some(view)) = (stream, view) else {
            return Err(self.type_error("This BYOB request has been invalidated"));
        };
        if view_of(&view).is_none_or(|view| view.buffer.borrow().detached) {
            return Err(self.type_error(
                "The BYOB request's buffer has been detached and so cannot be used as a response",
            ));
        }
        Ok(stream)
    }

    fn respond(
        &mut self,
        request: &Rc<RefCell<ByobRequest>>,
        written: usize,
    ) -> Result<(), ScriptError> {
        let stream = self.request_stream(request)?;
        let closed = matches!(stream.borrow().state, State::Closed);
        let (filled, length) = {
            let mut borrowed = stream.borrow_mut();
            let head = byte_source(&mut borrowed)
                .pending
                .front()
                .expect("a pending read");
            (head.filled, head.length)
        };
        if closed && written != 0 {
            return Err(
                self.type_error("bytesWritten must be 0 when calling respond() on a closed stream")
            );
        }
        if !closed && written == 0 {
            return Err(self.type_error(
                "bytesWritten must be greater than 0 when calling respond() on a readable stream",
            ));
        }
        if filled + written > length {
            return Err(self.range_error("bytesWritten out of range"));
        }
        {
            let mut borrowed = stream.borrow_mut();
            let head = byte_source(&mut borrowed)
                .pending
                .front_mut()
                .expect("a pending read");
            head.buffer = transfer(&head.buffer);
        }
        self.respond_internal(&stream, written)
    }

    fn respond_with_new_view(
        &mut self,
        request: &Rc<RefCell<ByobRequest>>,
        view: View,
    ) -> Result<(), ScriptError> {
        let stream = self.request_stream(request)?;
        if view.buffer.borrow().detached {
            return Err(self.type_error(
                "The given view's buffer has been detached and so cannot be used as a response",
            ));
        }
        let closed = matches!(stream.borrow().state, State::Closed);
        if closed && view.length != 0 {
            return Err(self.type_error(
                "The view's length must be 0 when calling respondWithNewView() on a closed stream",
            ));
        }
        if !closed && view.length == 0 {
            return Err(self.type_error("The view's length must be greater than 0 when calling respondWithNewView() on a readable stream"));
        }
        let (offset, filled, length, buffer_length) = {
            let mut borrowed = stream.borrow_mut();
            let head = byte_source(&mut borrowed)
                .pending
                .front()
                .expect("a pending read");
            (head.offset, head.filled, head.length, head.buffer_length)
        };
        if offset + filled != view.offset {
            return Err(self.range_error("The region specified by view does not match byobRequest"));
        }
        if buffer_length != view.buffer.borrow().len() {
            return Err(
                self.range_error("The buffer of view has different capacity than byobRequest")
            );
        }
        if filled + view.length > length {
            return Err(self.range_error("The region specified by view is larger than byobRequest"));
        }
        let moved = transfer(&view.buffer);
        {
            let mut borrowed = stream.borrow_mut();
            byte_source(&mut borrowed)
                .pending
                .front_mut()
                .expect("a pending read")
                .buffer = moved;
        }
        self.respond_internal(&stream, view.length)
    }

    fn respond_internal(
        &mut self,
        stream: &Rc<RefCell<Readable>>,
        written: usize,
    ) -> Result<(), ScriptError> {
        invalidate_request(stream);
        if matches!(stream.borrow().state, State::Closed) {
            let released = byte_source(&mut stream.borrow_mut())
                .pending
                .front()
                .is_some_and(|head| head.taker == Taker::Released);
            if released {
                pop_pending(stream);
            }
            let reader = stream.borrow().reader.clone();
            if let Some(reader) = reader.filter(|reader| reader.borrow().byob) {
                while !reader.borrow().requests.is_empty() {
                    let Some(descriptor) = pop_pending(stream) else {
                        break;
                    };
                    self.commit_pull_into(stream, descriptor);
                }
            }
        } else {
            let (taker, short) = {
                let mut borrowed = stream.borrow_mut();
                let head = byte_source(&mut borrowed)
                    .pending
                    .front_mut()
                    .expect("a pending read");
                head.filled += written;
                (head.taker, head.filled < head.minimum)
            };
            if taker == Taker::Released {
                self.queue_released_head(stream);
                self.fill_pending_from_queue(stream);
            } else if !short {
                let mut descriptor = pop_pending(stream).expect("a pending read");
                let remainder = descriptor.filled % descriptor.kind.size();
                if remainder > 0 {
                    let end = descriptor.offset + descriptor.filled;
                    queue_copy(stream, &descriptor.buffer, end - remainder, remainder);
                }
                descriptor.filled -= remainder;
                self.commit_pull_into(stream, descriptor);
                self.fill_pending_from_queue(stream);
            }
        }
        self.call_pull_if_needed(stream)
    }

    // ReadableByteStreamController's release steps: only the head pull-into stays, waited on
    // by no one
    pub(super) fn release_byte_reads(stream: &mut Readable) {
        if let Some(source) = stream.bytes.as_mut()
            && let Some(mut head) = source.pending.pop_front()
        {
            head.taker = Taker::Released;
            source.pending.clear();
            source.pending.push_back(head);
        }
    }

    // erroring or canceling a byte stream drops its queue and every pull-into
    pub(super) fn clear_byte_reads(stream: &Rc<RefCell<Readable>>) {
        invalidate_request(stream);
        if let Some(source) = stream.borrow_mut().bytes.as_mut() {
            source.pending.clear();
            source.chunks.clear();
        }
    }

    pub(super) fn call_byte_stream(
        &mut self,
        function: StreamFunction,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        use StreamFunction as F;
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        match function {
            F::ByteDesiredSize | F::ByteEnqueue | F::ByteClose | F::ByteError | F::ByobRequest => {
                let Some(StreamHandle::ByteController(stream)) = Self::handle_of(&this) else {
                    return Err(
                        self.type_error("Illegal invocation: not a ReadableByteStreamController")
                    );
                };
                match function {
                    F::ByteDesiredSize => {
                        Ok(Self::desired_size(&stream.borrow()).map_or(Value::Null, Value::Number))
                    }
                    F::ByobRequest => Ok(self.byob_request(&stream)),
                    F::ByteEnqueue => {
                        let failed =
                            "Failed to execute 'enqueue' on 'ReadableByteStreamController'";
                        let Some(view) = view_of(&first) else {
                            return Err(self.type_error(format!(
                                "{failed}: parameter 1 is not of type 'ArrayBufferView'."
                            )));
                        };
                        if view.length == 0 {
                            return Err(self.type_error(format!("{failed}: chunk is empty.")));
                        }
                        if view.buffer.borrow().is_empty() {
                            return Err(
                                self.type_error(format!("{failed}: chunk's buffer is empty."))
                            );
                        }
                        if stream.borrow().close_requested
                            || !matches!(stream.borrow().state, State::Readable)
                        {
                            return Err(self.type_error(format!("{failed}: Cannot enqueue a chunk into a readable stream that is closed or has been requested to be closed")));
                        }
                        self.byte_enqueue(&stream, view)?;
                        Ok(Value::Undefined)
                    }
                    F::ByteClose => {
                        if stream.borrow().close_requested
                            || !matches!(stream.borrow().state, State::Readable)
                        {
                            return Err(self.type_error("Cannot close a readable stream that has already been requested to be closed"));
                        }
                        match self.byte_close(&stream) {
                            Some(error) => Err(self.throw_value(error)),
                            None => Ok(Value::Undefined),
                        }
                    }
                    _ => {
                        self.error_readable(&stream, first);
                        Ok(Value::Undefined)
                    }
                }
            }
            F::ByobRequestView | F::Respond | F::RespondWithNewView => {
                let Some(StreamHandle::ByobRequest(request)) = Self::handle_of(&this) else {
                    return Err(
                        self.type_error("Illegal invocation: not a ReadableStreamBYOBRequest")
                    );
                };
                match function {
                    F::ByobRequestView => Ok(request.borrow().view.clone().unwrap_or(Value::Null)),
                    F::Respond => {
                        let written = self.enforce_unsigned(first, "bytesWritten")?;
                        self.respond(&request, written)?;
                        Ok(Value::Undefined)
                    }
                    _ => {
                        let Some(view) = view_of(&first) else {
                            return Err(self.type_error("Failed to execute 'respondWithNewView' on 'ReadableStreamBYOBRequest': parameter 1 is not of type 'ArrayBufferView'."));
                        };
                        self.respond_with_new_view(&request, view)?;
                        Ok(Value::Undefined)
                    }
                }
            }
            F::ByobRead | F::ByobReleaseLock | F::ByobCancel | F::ByobClosed => {
                let Some(StreamHandle::ByobReader(reader)) = Self::handle_of(&this) else {
                    let error =
                        self.type_error("Illegal invocation: not a ReadableStreamBYOBReader");
                    if matches!(function, F::ByobReleaseLock) {
                        return Err(error);
                    }
                    let reason = self.take_thrown(error);
                    return Ok(Value::Promise(self.rejected_promise_value(reason)));
                };
                match function {
                    F::ByobRead if arguments.is_empty() => {
                        let error = self.type_error("Failed to execute 'read' on 'ReadableStreamBYOBReader': 1 argument required, but only 0 present.");
                        let reason = self.take_thrown(error);
                        Ok(Value::Promise(self.rejected_promise_value(reason)))
                    }
                    F::ByobRead => {
                        let options = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                        self.byob_read(&reader, first, options)
                    }
                    F::ByobReleaseLock => {
                        self.release_reader(&reader);
                        Ok(Value::Undefined)
                    }
                    F::ByobClosed => Ok(Value::Promise(reader.borrow().closed.clone())),
                    _ => {
                        let Some(stream) = reader.borrow().stream.clone() else {
                            return Ok(Value::Promise(self.rejected_promise(
                                "This readable stream reader has been released",
                            )));
                        };
                        Ok(Value::Promise(self.cancel_readable(&stream, first)?))
                    }
                }
            }
            _ => unreachable!("only byte stream functions come here"),
        }
    }
}
