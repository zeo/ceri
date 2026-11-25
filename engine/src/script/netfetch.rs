// fetches to outside origins that the host runs off the page's thread: the promise settles
// when the head arrives, and the body streams in as the host reports its chunks

use super::realm::Native;
use super::streams::Readable;
use super::*;
use crate::navigation::{ExternalEvent, ExternalReport, ExternalRequest};
use std::cell::Cell;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

// a response body as it streams in, and the flag its cancel sets
pub(super) type ArrivingBody = (Rc<RefCell<Readable>>, Rc<Cell<bool>>);

pub(super) struct PendingFetch {
    id: u32,
    url: String,
    events: Receiver<ExternalEvent>,
    promise: PromiseHandle,
    // the response's body once the head has come
    body: Option<ArrivingBody>,
    signal: Option<AbortSignalHandle>,
}

impl PendingFetch {
    pub(super) fn trace(&self, tracer: &mut super::heap::Tracer) {
        tracer.trace_promise(&self.promise);
        if let Some((readable, _)) = &self.body {
            super::streams::trace_readable(readable, tracer);
        }
        if let Some(signal) = &self.signal {
            tracer.trace_abort_signal(signal);
        }
    }
}

impl ScriptRuntime {
    // hands the request to the host's thread when it takes one; none leaves it to be answered in place
    pub(super) fn spawn_fetch(
        &self,
        provider: &dyn ResourceProvider,
        url: String,
        external: &ExternalRequest,
        signal: Option<AbortSignalHandle>,
    ) -> Option<PromiseHandle> {
        let (sender, events) = channel();
        let waker = self.socket_waker();
        let report: ExternalReport = Box::new(move |event| {
            let _ = sender.send(event);
            if let Some(waker) = &waker {
                waker();
            }
        });
        if !provider.spawn_external(external, report) {
            return None;
        }
        let promise = Self::pending_promise();
        let id = self.next_fetch_id.get();
        self.next_fetch_id.set(id.wrapping_add(1));
        if let Some(signal) = &signal {
            let listener = self.native_function(Native::FetchAbort(id), "", 1);
            signal.borrow_mut().listeners.push(listener);
        }
        self.fetches.borrow_mut().push(PendingFetch {
            id,
            url,
            events,
            promise: promise.clone(),
            body: None,
            signal,
        });
        Some(promise)
    }

    // what the host has reported for the fetches in flight since the last turn
    pub(super) fn pump_fetches(&mut self) -> Result<usize, ScriptError> {
        if self.fetches.borrow().is_empty() {
            return Ok(0);
        }
        let pending = std::mem::take(&mut *self.fetches.borrow_mut());
        let mut processed = 0;
        let mut kept = Vec::new();
        for mut fetch in pending {
            let mut finished = false;
            loop {
                let event = match fetch.events.try_recv() {
                    Ok(event) => event,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        ExternalEvent::Failed("the connection closed early".to_string())
                    }
                };
                processed += 1;
                match event {
                    ExternalEvent::Head {
                        status,
                        mime_type,
                        headers,
                    } => {
                        let response = Rc::new(response_data(
                            fetch.url.clone(),
                            status,
                            String::new(),
                            mime_type,
                            headers_handle(headers, false),
                            Vec::new(),
                            ResponseMetadata {
                                redirected: false,
                                response_type: "cors".to_string(),
                            },
                        ));
                        let (stream, readable, canceled) = self.network_stream();
                        if let Value::Object(stream) = stream {
                            response.stream.borrow_mut().object = Some(stream);
                        }
                        fetch.body = Some((readable, canceled));
                        self.settle_promise(
                            &fetch.promise,
                            PromiseOutcome::Fulfilled(Value::Response(response)),
                        );
                    }
                    ExternalEvent::Chunk(bytes) => {
                        if let Some((readable, canceled)) = &fetch.body {
                            if canceled.get() {
                                finished = true;
                                break;
                            }
                            self.network_chunk(readable, bytes)?;
                        }
                    }
                    ExternalEvent::Done => {
                        if let Some((readable, _)) = &fetch.body {
                            self.network_done(readable);
                        }
                        finished = true;
                        break;
                    }
                    ExternalEvent::Failed(message) => {
                        let reason =
                            self.make_error("TypeError", &format!("Failed to fetch: {message}"));
                        match &fetch.body {
                            Some((readable, _)) => self.network_failed(readable, reason),
                            None => self
                                .settle_promise(&fetch.promise, PromiseOutcome::Rejected(reason)),
                        }
                        finished = true;
                        break;
                    }
                }
            }
            if !finished {
                kept.push(fetch);
            }
        }
        let mut fetches = self.fetches.borrow_mut();
        kept.append(&mut fetches);
        *fetches = kept;
        Ok(processed)
    }

    // an aborted fetch rejects, or errors its body when that has started to arrive
    pub(super) fn abort_fetch(&mut self, id: u32) -> Result<Value, ScriptError> {
        let found = {
            let mut fetches = self.fetches.borrow_mut();
            fetches
                .iter()
                .position(|fetch| fetch.id == id)
                .map(|index| fetches.remove(index))
        };
        let Some(fetch) = found else {
            return Ok(Value::Undefined);
        };
        let reason = fetch
            .signal
            .as_ref()
            .and_then(|signal| signal.borrow().reason.clone())
            .unwrap_or_else(abort_error_value);
        match &fetch.body {
            Some((readable, _)) => self.network_failed(readable, reason),
            None => self.settle_promise(&fetch.promise, PromiseOutcome::Rejected(reason)),
        }
        Ok(Value::Undefined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::{NavigationError, Resource, ResourceBundle};
    use std::time::{Duration, Instant};

    // answers on a thread of its own, the body in two pieces with a pause between
    struct Streaming;

    impl ResourceProvider for Streaming {
        fn provide(&self, _url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
            Ok(None)
        }

        fn spawn_external(&self, request: &ExternalRequest, report: ExternalReport) -> bool {
            let slow = request.url().contains("slow");
            std::thread::spawn(move || {
                if slow {
                    std::thread::sleep(Duration::from_millis(200));
                }
                report(ExternalEvent::Head {
                    status: 200,
                    mime_type: "text/plain".to_string(),
                    headers: BTreeMap::new(),
                });
                report(ExternalEvent::Chunk(b"da".to_vec()));
                std::thread::sleep(Duration::from_millis(30));
                report(ExternalEvent::Chunk(b"ta".to_vec()));
                report(ExternalEvent::Done);
            });
            true
        }
    }

    #[test]
    fn outside_fetches_answer_off_the_thread_and_stream_their_bodies() {
        let document =
            crate::dom::Document::parse(r#"<p id="out">idle</p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let base = AppUrl::parse("https://app.ceri.invalid/").expect("base URL should parse");
        let mut runtime = ScriptRuntime::with_resources_and_storage_and_runtime_info(
            document.clone(),
            ResourceBundle::default(),
            Some(Rc::new(Streaming)),
            base,
            Rc::new(RefCell::new(StorageData::new())),
            Rc::new(RefCell::new(StorageData::new())),
            None,
        );
        runtime
            .execute(
                r#"
                const out = document.getElementById("out");
                const seen = [];
                (async () => {
                    const response = await fetch("https://api.example.test/stream");
                    seen.push(response.status);
                    for await (const chunk of response.body.pipeThrough(new TextDecoderStream())) seen.push(chunk);
                    const again = await (await fetch("https://api.example.test/text")).text();
                    seen.push(again);
                    const controller = new AbortController();
                    const slow = fetch("https://api.example.test/slow", { signal: controller.signal });
                    controller.abort();
                    try { await slow; } catch (error) { seen.push(error.name); }
                    out.textContent = seen.join("|");
                })();
                "#,
            )
            .expect("script should run");
        let text = |document: &Rc<RefCell<crate::dom::Document>>| {
            let node = document.borrow().find_element_by_id("out").unwrap();
            document
                .borrow()
                .element_text_content(node)
                .unwrap_or_default()
        };
        assert_eq!(
            text(&document),
            "idle",
            "the page thread is not held while the host answers"
        );
        let started = Instant::now();
        while text(&document) == "idle" && started.elapsed() < Duration::from_secs(5) {
            runtime.pump().expect("the pump should run");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(text(&document), "200|da|ta|data|AbortError");
    }
}
