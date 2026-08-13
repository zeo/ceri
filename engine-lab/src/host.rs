use crate::layout::Size;
use crate::navigation::{Browser, NavigationError, NavigationFailure, NavigationStatus};
use crate::paint::Canvas;
use crate::AccessibilityNode;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::thread;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    Primary,
    Secondary,
    Auxiliary,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    Resize(Size),
    PointerMove {
        x: f32,
        y: f32,
    },
    PointerDown {
        x: f32,
        y: f32,
        button: PointerButton,
    },
    PointerUp {
        x: f32,
        y: f32,
        button: PointerButton,
    },
    Click {
        x: f32,
        y: f32,
    },
    KeyDown {
        key: String,
    },
    TextInput {
        text: String,
    },
    Blur,
    AdvanceTime(u64),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputResult {
    pub handled: bool,
    pub tasks_run: usize,
    pub frame_requested: bool,
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub sequence: u64,
    pub viewport: Size,
    pub canvas: Canvas,
}

pub struct EngineHost {
    browser: Browser,
    viewport: Size,
    next_frame: u64,
    dirty: bool,
}

#[derive(Debug)]
pub enum EngineWorkerEvent {
    Navigated {
        request_id: u64,
        result: Result<NavigationStatus, NavigationError>,
    },
    Input {
        request_id: u64,
        result: Result<InputResult, NavigationError>,
    },
    Frame {
        request_id: u64,
        result: Result<Option<Frame>, NavigationError>,
    },
    Accessibility {
        request_id: u64,
        tree: Option<AccessibilityNode>,
    },
    NavigationFailure {
        request_id: u64,
        failure: Option<NavigationFailure>,
    },
}

enum EngineWorkerCommand {
    Navigate { request_id: u64, url: String },
    Input {
        request_id: u64,
        event: InputEvent,
    },
    Render { request_id: u64 },
    Accessibility { request_id: u64 },
    NavigationFailure { request_id: u64 },
    Shutdown,
}

pub struct EngineWorker {
    commands: SyncSender<EngineWorkerCommand>,
    events: Receiver<EngineWorkerEvent>,
    next_request_id: u64,
}

impl EngineWorker {
    pub fn spawn<F>(viewport: Size, factory: F) -> Result<Self, String>
    where
        F: FnOnce() -> Result<Browser, String> + Send + 'static,
    {
        let (commands, command_receiver) = mpsc::sync_channel(64);
        let (events, event_receiver) = mpsc::channel();
        let (ready, ready_receiver) = mpsc::channel();
        thread::Builder::new()
            .name("ceri-engine".to_string())
            .spawn(move || {
                let browser = match factory() {
                    Ok(browser) => browser,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                let _ = ready.send(Ok(()));
                run_engine_worker(
                    EngineHost::new(browser, viewport),
                    command_receiver,
                    events,
                );
            })
            .map_err(|error| format!("could not start engine worker: {error}"))?;
        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                commands,
                events: event_receiver,
                next_request_id: 1,
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err("engine worker stopped during initialization".to_string()),
        }
    }

    pub fn navigate(&mut self, url: impl Into<String>) -> Result<u64, String> {
        let request_id = self.next_request();
        self.queue(EngineWorkerCommand::Navigate {
            request_id,
            url: url.into(),
        })?;
        Ok(request_id)
    }

    pub fn input(&mut self, event: InputEvent) -> Result<u64, String> {
        let request_id = self.next_request();
        self.queue(EngineWorkerCommand::Input { request_id, event })?;
        Ok(request_id)
    }

    pub fn render(&mut self) -> Result<u64, String> {
        let request_id = self.next_request();
        self.queue(EngineWorkerCommand::Render { request_id })?;
        Ok(request_id)
    }

    pub fn accessibility_tree(&mut self) -> Result<u64, String> {
        let request_id = self.next_request();
        self.queue(EngineWorkerCommand::Accessibility { request_id })?;
        Ok(request_id)
    }

    pub fn last_navigation_failure(&mut self) -> Result<u64, String> {
        let request_id = self.next_request();
        self.queue(EngineWorkerCommand::NavigationFailure { request_id })?;
        Ok(request_id)
    }

    pub fn poll(&self) -> Vec<EngineWorkerEvent> {
        self.events.try_iter().collect()
    }

    pub fn wait(&self) -> Result<EngineWorkerEvent, String> {
        self.events
            .recv()
            .map_err(|_| "engine worker stopped".to_string())
    }

    pub fn shutdown(self) {
        let _ = self.commands.try_send(EngineWorkerCommand::Shutdown);
    }

    fn queue(&self, command: EngineWorkerCommand) -> Result<(), String> {
        match self.commands.try_send(command) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err("engine worker queue is full".to_string()),
            Err(TrySendError::Disconnected(_)) => {
                Err("engine worker is unavailable".to_string())
            }
        }
    }

    fn next_request(&mut self) -> u64 {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        request_id
    }
}

fn run_engine_worker(
    mut host: EngineHost,
    commands: Receiver<EngineWorkerCommand>,
    events: Sender<EngineWorkerEvent>,
) {
    while let Ok(command) = commands.recv() {
        let event = match command {
            EngineWorkerCommand::Navigate { request_id, url } => {
                EngineWorkerEvent::Navigated {
                    request_id,
                    result: host.navigate(&url),
                }
            }
            EngineWorkerCommand::Input { request_id, event } => EngineWorkerEvent::Input {
                request_id,
                result: host.input(event),
            },
            EngineWorkerCommand::Render { request_id } => EngineWorkerEvent::Frame {
                request_id,
                result: host.render(),
            },
            EngineWorkerCommand::Accessibility { request_id } => {
                EngineWorkerEvent::Accessibility {
                    request_id,
                    tree: host.accessibility_tree(),
                }
            }
            EngineWorkerCommand::NavigationFailure { request_id } => {
                EngineWorkerEvent::NavigationFailure {
                    request_id,
                    failure: host.last_navigation_failure().cloned(),
                }
            }
            EngineWorkerCommand::Shutdown => break,
        };
        if events.send(event).is_err() {
            break;
        }
    }
}

impl EngineHost {
    pub fn new(browser: Browser, viewport: Size) -> Self {
        Self {
            browser,
            viewport,
            next_frame: 1,
            dirty: true,
        }
    }

    pub fn browser(&self) -> &Browser {
        &self.browser
    }

    pub fn viewport(&self) -> Size {
        self.viewport
    }

    pub fn accessibility_tree(&self) -> Option<AccessibilityNode> {
        self.browser.accessibility_tree()
    }

    pub fn last_navigation_failure(&self) -> Option<&NavigationFailure> {
        self.browser.last_navigation_failure()
    }

    pub fn navigate(&mut self, url: &str) -> Result<NavigationStatus, NavigationError> {
        let status = self.browser.navigate(url)?;
        self.dirty = true;
        Ok(status)
    }

    pub fn click_id(&mut self, id: &str) -> Result<bool, NavigationError> {
        let handled = self.browser.click_id(id)?;
        self.dirty |= handled;
        Ok(handled)
    }

    pub fn go(&mut self, delta: isize) -> Result<bool, NavigationError> {
        let handled = self.browser.go(delta)?;
        self.dirty |= handled;
        Ok(handled)
    }

    pub fn back(&mut self) -> Result<bool, NavigationError> {
        self.go(-1)
    }

    pub fn forward(&mut self) -> Result<bool, NavigationError> {
        self.go(1)
    }

    pub fn input(&mut self, event: InputEvent) -> Result<InputResult, NavigationError> {
        self.pump_workers()?;
        let result = match event {
            InputEvent::Resize(viewport) => {
                self.viewport = viewport;
                InputResult {
                    frame_requested: true,
                    ..InputResult::default()
                }
            }
            InputEvent::PointerMove { x, y } => InputResult {
                handled: self
                    .browser
                    .dispatch_at(self.viewport, x, y, "pointermove")?,
                ..InputResult::default()
            },
            InputEvent::PointerDown { x, y, button: _ } => InputResult {
                handled: self
                    .browser
                    .dispatch_at(self.viewport, x, y, "pointerdown")?,
                ..InputResult::default()
            },
            InputEvent::PointerUp { x, y, button: _ } => InputResult {
                handled: self.browser.dispatch_at(self.viewport, x, y, "pointerup")?,
                ..InputResult::default()
            },
            InputEvent::Click { x, y } => InputResult {
                handled: self.browser.click_at(self.viewport, x, y)?,
                ..InputResult::default()
            },
            InputEvent::KeyDown { key } => InputResult {
                handled: self.browser.key_down(&key)?,
                ..InputResult::default()
            },
            InputEvent::TextInput { text } => InputResult {
                handled: self.browser.insert_text(&text)?,
                ..InputResult::default()
            },
            InputEvent::Blur => InputResult {
                handled: self.browser.blur()?,
                ..InputResult::default()
            },
            InputEvent::AdvanceTime(milliseconds) => InputResult {
                tasks_run: self.browser.advance_time(milliseconds)?,
                ..InputResult::default()
            },
        };
        self.pump_workers()?;
        self.dirty |= result.frame_requested || result.handled || result.tasks_run > 0;
        Ok(InputResult {
            frame_requested: self.dirty,
            ..result
        })
    }

    pub fn render(&mut self) -> Result<Option<Frame>, NavigationError> {
        self.pump_workers()?;
        if !self.dirty {
            return Ok(None);
        }
        let canvas = self.browser.paint(self.viewport)?;
        let frame = Frame {
            sequence: self.next_frame,
            viewport: self.viewport,
            canvas,
        };
        self.next_frame = self.next_frame.saturating_add(1);
        self.dirty = false;
        Ok(Some(frame))
    }

    fn pump_workers(&mut self) -> Result<(), NavigationError> {
        if self.browser.current_url().is_some() {
            self.dirty |= self.browser.pump()? > 0;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::{
        AppUrl, Resource, ResourceBundle, ResourceProvider, ResourceRequest, ResourceResponse,
        APP_ORIGIN,
    };
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};
    use std::thread::ThreadId;

    fn bundle(source: &str) -> ResourceBundle {
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/index.html",
                "text/html; charset=utf-8",
                source.as_bytes().to_vec(),
            )
            .expect("HTML resource should be accepted");
        resources
    }

    struct ThreadRecordingProvider {
        thread: Arc<Mutex<Option<ThreadId>>>,
    }

    impl ResourceProvider for ThreadRecordingProvider {
        fn provide(&self, _url: &AppUrl) -> Result<Option<Resource>, NavigationError> {
            Ok(None)
        }

        fn provide_response(
            &self,
            request: &ResourceRequest,
        ) -> Result<Option<ResourceResponse>, NavigationError> {
            *self.thread.lock().expect("thread marker should not be poisoned") =
                Some(std::thread::current().id());
            if request.url().path() != "/" {
                return Ok(None);
            }
            ResourceResponse::new(
                200,
                "text/html",
                BTreeMap::new(),
                b"<main><p id='status'>worker</p></main>".to_vec(),
            )
            .map(Some)
        }
    }

    #[test]
    fn host_runs_navigation_input_and_frame_lifecycle() {
        let browser = Browser::new(bundle(
            r#"<main><button id="go" style="display: block; width: 120px; height: 40px">run</button><p id="status">idle</p></main><script>
                const button = document.getElementById("go");
                const status = document.getElementById("status");
                button.addEventListener("click", () => { status.textContent = "clicked"; });
                setTimeout(() => { status.textContent = "timer"; }, 25);
            </script>"#,
        ));
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 320.0,
                height: 180.0,
            },
        );
        assert_eq!(
            host.navigate("https://app.ceri.invalid/")
                .expect("page should load"),
            NavigationStatus::Loaded
        );
        let first = host
            .render()
            .expect("initial frame should render")
            .expect("initial frame should be present");
        assert_eq!(first.sequence, 1);
        assert_eq!(first.canvas.width(), 320);
        assert_eq!(
            first.canvas.rgba_pixels().len(),
            first.canvas.bytes_per_row() * 180
        );
        assert!(host.render().expect("clean host should render").is_none());

        let click = host
            .input(InputEvent::Click { x: 10.0, y: 10.0 })
            .expect("click should dispatch");
        assert!(click.handled);
        let second = host
            .render()
            .expect("clicked frame should render")
            .expect("clicked frame should be present");
        assert_eq!(second.sequence, 2);

        let tick = host
            .input(InputEvent::AdvanceTime(25))
            .expect("timer should advance");
        assert_eq!(tick.tasks_run, 1);
        let third = host
            .render()
            .expect("timer frame should render")
            .expect("timer frame should be present");
        assert_eq!(third.sequence, 3);

        host.input(InputEvent::Resize(Size {
            width: 640.0,
            height: 240.0,
        }))
        .expect("resize should be accepted");
        let resized = host
            .render()
            .expect("resized frame should render")
            .expect("resized frame should be present");
        assert_eq!(resized.viewport.width, 640.0);
        assert_eq!(resized.canvas.height(), 240);
    }

    #[test]
    fn host_render_pumps_packaged_worker_callbacks() {
        let mut resources = bundle(
            r#"<p id='status'>idle</p><script>
              const status = document.getElementById("status");
              const worker = new Worker("/worker.js");
              worker.onmessage = (event) => { status.textContent = event.data; };
              worker.postMessage("ready");
            </script>"#,
        );
        resources
            .insert(
                "/worker.js",
                "text/javascript",
                br#"self.onmessage = (event) => { postMessage(event.data + "!"); };"#
                    .to_vec(),
            )
            .expect("worker resource should be accepted");
        let mut host = EngineHost::new(
            Browser::new(resources),
            Size {
                width: 320.0,
                height: 200.0,
            },
        );
        host.navigate("https://app.ceri.invalid/index.html")
            .expect("packaged document should load");
        for _ in 0..100 {
            let _ = host.render().expect("worker frame should render");
            let document = host.browser().document().expect("document should exist");
            let status = document
                .borrow()
                .find_element_by_id("status")
                .expect("status should exist");
            if document.borrow().element_text_content(&status).as_deref() == Some("ready!") {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("host render loop did not deliver the worker callback");
    }

    #[test]
    fn host_exposes_the_current_accessibility_tree() {
        let browser = Browser::new(bundle(
            r#"<main><button aria-label="Run">run</button><p hidden>ignored</p></main>"#,
        ));
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 320.0,
                height: 180.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let tree = host
            .accessibility_tree()
            .expect("loaded page should expose an accessibility tree");
        assert_eq!(tree.role, "document");
        assert_eq!(tree.children[0].role, "main");
        assert_eq!(tree.children[0].children.len(), 1);
        assert_eq!(tree.children[0].children[0].name, "Run");
    }

    #[test]
    fn host_preserves_pointer_event_order_at_a_layout_target() {
        let browser = Browser::new(bundle(
            r#"<button id="go" style="display: block; width: 120px; height: 40px">run</button><p id="status">idle</p><script>
                const button = document.getElementById("go");
                const status = document.getElementById("status");
                button.addEventListener("pointerdown", () => { status.textContent = "down"; });
                button.addEventListener("pointerup", () => { status.textContent = status.textContent + " up"; });
                button.addEventListener("click", () => { status.textContent = status.textContent + " click"; });
            </script>"#,
        ));
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 320.0,
                height: 180.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        host.render()
            .expect("initial frame should render")
            .expect("initial frame should be present");
        assert!(
            host.input(InputEvent::PointerDown {
                x: 10.0,
                y: 10.0,
                button: PointerButton::Primary,
            })
            .expect("pointerdown should dispatch")
            .handled
        );
        assert!(
            host.input(InputEvent::PointerUp {
                x: 10.0,
                y: 10.0,
                button: PointerButton::Primary,
            })
            .expect("pointerup should dispatch")
            .handled
        );
        assert!(
            host.input(InputEvent::Click { x: 10.0, y: 10.0 })
                .expect("click should dispatch")
                .handled
        );
        let status = host.browser().document().expect("document should exist");
        let status = status
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            host.browser()
                .document()
                .expect("document should exist")
                .borrow()
                .element_text_content(&status)
                .as_deref(),
            Some("down up click")
        );
    }

    #[test]
    fn host_routes_text_input_to_the_focused_control() {
        let browser = Browser::new(bundle(
            r#"<input id="name" value="R" style="display: block; width: 180px; height: 32px"><p id="status">idle</p><script>
                const input = document.getElementById("name");
                const status = document.getElementById("status");
                input.addEventListener("input", () => { status.textContent = input.value; });
            </script>"#,
        ));
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 320.0,
                height: 180.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        assert!(host
            .click_id("name")
            .expect("click should focus the input"));
        assert!(host
            .input(InputEvent::TextInput {
                text: "ust".to_string(),
            })
            .expect("text should be inserted")
            .handled);
        assert!(host
            .input(InputEvent::KeyDown {
                key: "Backspace".to_string(),
            })
            .expect("backspace should be handled")
            .handled);
        let document = host.browser().document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("Rus")
        );
    }

    #[test]
    fn host_exposes_session_history_traversal() {
        let mut resources = bundle(r#"<a id="go" href="/about.html">about</a>"#);
        resources
            .insert(
                "/about.html",
                "text/html",
                b"<p id='status'>about</p>".to_vec(),
            )
            .expect("about page should be accepted");
        let mut host = EngineHost::new(
            Browser::new(resources),
            Size {
                width: 320.0,
                height: 180.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("home should load");
        assert!(host.click_id("go").expect("link should activate"));
        assert_eq!(
            host.browser().current_url().expect("URL should exist").path(),
            "/about.html"
        );
        assert!(host.back().expect("history back should run"));
        assert_eq!(
            host.browser().current_url().expect("URL should exist").path(),
            "/"
        );
        assert!(host.render().expect("history frame should render").is_some());
    }

    #[test]
    fn worker_keeps_blocking_resource_work_off_the_calling_thread() {
        let caller = std::thread::current().id();
        let provider_thread = Arc::new(Mutex::new(None));
        let marker = provider_thread.clone();
        let mut worker = EngineWorker::spawn(
            Size {
                width: 320.0,
                height: 180.0,
            },
            move || {
                Ok(Browser::with_resource_provider(
                    ResourceBundle::default(),
                    Rc::new(ThreadRecordingProvider { thread: marker }),
                ))
            },
        )
        .expect("engine worker should start");
        let request_id = worker
            .navigate(format!("{APP_ORIGIN}/"))
            .expect("navigation should queue");
        match worker.wait().expect("worker should report navigation") {
            EngineWorkerEvent::Navigated {
                request_id: completed,
                result,
            } => {
                assert_eq!(completed, request_id);
                assert_eq!(result.expect("navigation should load"), NavigationStatus::Loaded);
            }
            other => panic!("unexpected worker event: {other:?}"),
        }
        let provider_thread = provider_thread
            .lock()
            .expect("thread marker should not be poisoned")
            .expect("provider should record its thread");
        assert_ne!(provider_thread, caller);
        worker.shutdown();
    }

    #[test]
    fn worker_preserves_command_order_and_returns_frames() {
        let mut worker = EngineWorker::spawn(
            Size {
                width: 320.0,
                height: 180.0,
            },
            move || {
                Ok(Browser::new(bundle(
                    "<button id='go' style='display: block; width: 100px; height: 30px'>go</button>",
                )))
            },
        )
        .expect("engine worker should start");
        let navigate = worker
            .navigate(format!("{APP_ORIGIN}/"))
            .expect("navigation should queue");
        let input = worker
            .input(InputEvent::Click { x: 4.0, y: 4.0 })
            .expect("input should queue");
        let render = worker.render().expect("render should queue");

        let first = worker.wait().expect("navigation event should arrive");
        let second = worker.wait().expect("input event should arrive");
        let third = worker.wait().expect("frame event should arrive");
        assert!(matches!(
            first,
            EngineWorkerEvent::Navigated {
                request_id,
                result: Ok(NavigationStatus::Loaded)
            } if request_id == navigate
        ));
        assert!(matches!(
            second,
            EngineWorkerEvent::Input {
                request_id,
                result: Ok(InputResult { handled: true, .. })
            } if request_id == input
        ));
        assert!(matches!(
            third,
            EngineWorkerEvent::Frame {
                request_id,
                result: Ok(Some(Frame { sequence: 1, .. }))
            } if request_id == render
        ));
        worker.shutdown();
    }
}
