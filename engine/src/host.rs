use crate::AccessibilityNode;
use crate::layout::Size;
use crate::navigation::{
    Browser, NavigationError, NavigationFailure, NavigationStatus, UserPreferences,
};
use crate::paint::Canvas;
pub use crate::script::KeyModifiers;
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
    Scroll {
        x: f32,
        y: f32,
        delta_x: f32,
        delta_y: f32,
    },
    KeyDown {
        key: String,
    },
    KeyDownWithModifiers {
        key: String,
        modifiers: KeyModifiers,
    },
    TextInput {
        text: String,
    },
    CompositionUpdate {
        text: String,
        cursor: Option<(usize, usize)>,
    },
    CompositionCommit {
        text: String,
    },
    CompositionCancel,
    Blur,
    ScaleChanged(f32),
    PreferencesChanged(UserPreferences),
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
    pub scale: f32,
    pub title: Option<String>,
    pub canvas: Canvas,
}

pub struct EngineHost {
    browser: Browser,
    viewport: Size,
    scale: f32,
    selecting: bool,
    selecting_thumb: Option<crate::ThumbDrag>,
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
    Input { request_id: u64, event: InputEvent },
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
                run_engine_worker(EngineHost::new(browser, viewport), command_receiver, events);
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
            Err(TrySendError::Disconnected(_)) => Err("engine worker is unavailable".to_string()),
        }
    }

    fn next_request(&mut self) -> u64 {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        request_id
    }
}

fn normalize_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

fn run_engine_worker(
    mut host: EngineHost,
    commands: Receiver<EngineWorkerCommand>,
    events: Sender<EngineWorkerEvent>,
) {
    while let Ok(command) = commands.recv() {
        let event = match command {
            EngineWorkerCommand::Navigate { request_id, url } => EngineWorkerEvent::Navigated {
                request_id,
                result: host.navigate(&url),
            },
            EngineWorkerCommand::Input { request_id, event } => EngineWorkerEvent::Input {
                request_id,
                result: host.input(event),
            },
            EngineWorkerCommand::Render { request_id } => EngineWorkerEvent::Frame {
                request_id,
                result: host.render(),
            },
            EngineWorkerCommand::Accessibility { request_id } => EngineWorkerEvent::Accessibility {
                request_id,
                tree: host.accessibility_tree(),
            },
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
        Self::with_scale(browser, viewport, 1.0)
    }

    pub fn with_scale(mut browser: Browser, viewport: Size, scale: f32) -> Self {
        let scale = normalize_scale(scale);
        browser.set_viewport(viewport, scale);
        Self {
            browser,
            viewport,
            scale,
            selecting: false,
            selecting_thumb: None,
            next_frame: 1,
            dirty: true,
        }
    }

    pub fn browser(&self) -> &Browser {
        &self.browser
    }

    pub fn browser_mut(&mut self) -> &mut Browser {
        &mut self.browser
    }

    pub fn viewport(&self) -> Size {
        self.viewport
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn accessibility_tree(&self) -> Option<AccessibilityNode> {
        self.browser.accessibility_tree()
    }

    pub fn title(&self) -> Option<String> {
        self.browser.title()
    }

    pub fn last_navigation_failure(&self) -> Option<&NavigationFailure> {
        self.browser.last_navigation_failure()
    }

    pub fn navigate(&mut self, url: &str) -> Result<NavigationStatus, NavigationError> {
        let status = self.browser.navigate(url)?;
        self.browser.sample_transitions(self.viewport)?;
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
                self.browser.set_viewport(viewport, self.scale);
                InputResult {
                    frame_requested: true,
                    ..InputResult::default()
                }
            }
            InputEvent::ScaleChanged(scale) => {
                self.scale = normalize_scale(scale);
                self.browser.set_viewport(self.viewport, self.scale);
                InputResult {
                    frame_requested: true,
                    ..InputResult::default()
                }
            }
            InputEvent::PreferencesChanged(preferences) => {
                let changed = self.browser.user_preferences() != preferences;
                self.browser.set_user_preferences(preferences);
                InputResult {
                    frame_requested: changed,
                    ..InputResult::default()
                }
            }
            InputEvent::PointerMove { x, y } => {
                let dispatched = self
                    .browser
                    .dispatch_at(self.viewport, x, y, "pointermove")?;
                let dragged = match &self.selecting_thumb {
                    Some(drag) => self.browser.drag_thumb(drag, x, y)?,
                    _ => false,
                };
                let extended =
                    self.selecting && self.browser.select_at(self.viewport, x, y, true)?;
                InputResult {
                    handled: dispatched || dragged || extended,
                    frame_requested: dragged || extended,
                    ..InputResult::default()
                }
            }
            InputEvent::PointerDown { x, y, button } => {
                let dispatched = self
                    .browser
                    .dispatch_at(self.viewport, x, y, "pointerdown")?;
                let thumb_drag = (button == PointerButton::Primary)
                    .then(|| self.browser.thumb_at(self.viewport, x, y))
                    .flatten();
                if let Some(drag) = &thumb_drag {
                    self.selecting_thumb = Some(drag.clone());
                }
                let selected = button == PointerButton::Primary
                    && self.browser.select_at(self.viewport, x, y, false)?;
                self.selecting = selected;
                InputResult {
                    handled: dispatched || thumb_drag.is_some() || selected,
                    frame_requested: thumb_drag.is_some() || selected,
                    ..InputResult::default()
                }
            }
            InputEvent::PointerUp { x, y, button } => {
                if button == PointerButton::Primary {
                    self.selecting = false;
                    self.selecting_thumb = None;
                }
                InputResult {
                    handled: self.browser.dispatch_at(self.viewport, x, y, "pointerup")?,
                    ..InputResult::default()
                }
            }
            InputEvent::Click { x, y } => InputResult {
                handled: self.browser.click_at(self.viewport, x, y)?,
                ..InputResult::default()
            },
            InputEvent::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                let handled = self
                    .browser
                    .scroll_at(self.viewport, x, y, delta_x, delta_y)?;
                InputResult {
                    handled,
                    frame_requested: handled,
                    ..InputResult::default()
                }
            }
            InputEvent::KeyDown { key } => InputResult {
                handled: self.browser.key_down(&key)?,
                ..InputResult::default()
            },
            InputEvent::KeyDownWithModifiers { key, modifiers } => InputResult {
                handled: self.browser.key_down_with_modifiers(&key, modifiers)?,
                ..InputResult::default()
            },
            InputEvent::TextInput { text } => InputResult {
                handled: self.browser.insert_text(&text)?,
                ..InputResult::default()
            },
            InputEvent::CompositionUpdate { text, cursor } => InputResult {
                handled: self.browser.composition_update(&text, cursor)?,
                ..InputResult::default()
            },
            InputEvent::CompositionCommit { text } => InputResult {
                handled: self.browser.composition_commit(&text)?,
                ..InputResult::default()
            },
            InputEvent::CompositionCancel => InputResult {
                handled: self.browser.composition_cancel()?,
                ..InputResult::default()
            },
            InputEvent::Blur => InputResult {
                handled: self.browser.blur()?,
                ..InputResult::default()
            },
            InputEvent::AdvanceTime(milliseconds) => {
                let tasks_run = self.browser.advance_time(milliseconds)?;
                InputResult {
                    tasks_run,
                    frame_requested: self.browser.has_active_animations(),
                    ..InputResult::default()
                }
            }
        };
        self.pump_workers()?;
        // state changes may start a transition, which needs frames until it finishes
        let transitioning = self.browser.sample_transitions(self.viewport)?;
        self.dirty |= result.frame_requested
            || result.handled
            || result.tasks_run > 0
            || transitioning
            || self.browser.has_active_animations();
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
        let canvas = self.browser.paint_scaled(self.viewport, self.scale)?;
        let frame = Frame {
            sequence: self.next_frame,
            viewport: self.viewport,
            scale: self.scale,
            title: self.browser.title(),
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
    use crate::css::Color;
    use crate::navigation::{
        APP_ORIGIN, AppUrl, Resource, ResourceBundle, ResourceProvider, ResourceRequest,
        ResourceResponse,
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
            *self
                .thread
                .lock()
                .expect("thread marker should not be poisoned") = Some(std::thread::current().id());
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
    fn host_routes_wheel_input_to_element_scrolling_and_frame_invalidation() {
        let browser = Browser::new(bundle(
            r#"<style>#viewport { width: 40px; height: 20px; overflow: auto; margin: 0; } #content { width: 40px; height: 80px; margin: 0; }</style><div id="viewport"><div id="content"></div></div>"#,
        ));
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 120.0,
                height: 100.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        host.render()
            .expect("initial frame should render")
            .expect("initial frame should be present");
        let scroll = host
            .input(InputEvent::Scroll {
                x: 10.0,
                y: 10.0,
                delta_x: 0.0,
                delta_y: 30.0,
            })
            .expect("wheel input should be accepted");
        assert!(scroll.handled);
        assert!(scroll.frame_requested);
        let frame = host
            .render()
            .expect("scroll should invalidate the frame")
            .expect("scrolled frame should be present");
        assert_eq!(frame.viewport.height, 100.0);
        let layout = host
            .browser()
            .layout(Size {
                width: 120.0,
                height: 100.0,
            })
            .expect("scrolled host page should layout");
        assert_eq!(layout.root.children[0].children[0].rect.y, -30.0);
    }

    #[test]
    fn host_keeps_sticky_content_at_the_scrollport_edge() {
        let browser = Browser::new(bundle(
            r#"<style>
                #viewport { width: 80px; height: 40px; overflow: auto; margin: 0; }
                #sticky { position: sticky; top: 0; width: 80px; height: 10px; margin: 0; }
                #spacer { width: 80px; height: 80px; margin: 0; }
            </style>
            <div id="viewport"><div id="sticky">sticky</div><div id="spacer"></div></div>
            <p id="status">idle</p>
            <script>
                document.getElementById("sticky").addEventListener("click", () => {
                    document.getElementById("status").textContent = "clicked";
                });
            </script>"#,
        ));
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 120.0,
                height: 80.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("sticky page should load");
        let scroll = host
            .input(InputEvent::Scroll {
                x: 10.0,
                y: 10.0,
                delta_x: 0.0,
                delta_y: 20.0,
            })
            .expect("sticky wheel input should be accepted");
        assert!(scroll.handled);
        assert!(scroll.frame_requested);
        let layout = host
            .browser()
            .layout(Size {
                width: 120.0,
                height: 80.0,
            })
            .expect("sticky page should layout");
        let sticky = &layout.root.children[0].children[0];
        assert_eq!(sticky.rect.y, -20.0);
        assert_eq!(sticky.visual_offset, (0.0, 20.0));
        let click = host
            .input(InputEvent::Click { x: 10.0, y: 4.0 })
            .expect("sticky click should be accepted");
        assert!(click.handled);
        let document = host.browser().document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("clicked")
        );
    }

    #[test]
    fn host_keeps_scrollbar_gutter_out_of_content_hit_testing() {
        let browser = Browser::new(bundle(
            r#"<style>
                #viewport { width: 40px; height: 40px; overflow: auto; margin: 0; }
                #content { width: 80px; height: 60px; margin: 0; }
            </style>
            <div id="viewport"><div id="content">run</div></div>
            <p id="status">idle</p>
            <script>
                document.getElementById("content").addEventListener("click", () => {
                    document.getElementById("status").textContent = "clicked";
                });
            </script>"#,
        ));
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 80.0,
                height: 80.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("scrollbar page should load");
        host.render()
            .expect("initial scrollbar frame should render")
            .expect("initial scrollbar frame should be present");
        let click = host
            .input(InputEvent::Click { x: 35.0, y: 20.0 })
            .expect("gutter click should be accepted");
        assert!(!click.handled);
        let document = host.browser().document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("idle")
        );
        let scroll = host
            .input(InputEvent::Scroll {
                x: 35.0,
                y: 20.0,
                delta_x: 0.0,
                delta_y: 10.0,
            })
            .expect("gutter wheel input should be accepted");
        assert!(scroll.handled);
        assert!(scroll.frame_requested);
        let layout = host
            .browser()
            .layout(Size {
                width: 80.0,
                height: 80.0,
            })
            .expect("scrolled scrollbar page should layout");
        assert_eq!(layout.root.children[0].scroll_offset, (0.0, 10.0));
    }

    #[test]
    fn dragging_the_scrollbar_thumb_scrolls_with_the_pointer() {
        let browser = Browser::new(bundle(
            r#"<style>
                #viewport { width: 60px; height: 60px; overflow: auto; margin: 0; }
                #content { width: 40px; height: 300px; margin: 0; }
            </style>
            <div id="viewport"><div id="content"></div></div>"#,
        ));
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 80.0,
                height: 80.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("scrollbar page should load");

        // the vertical thumb sits at the top of the track beside the viewport
        let press = InputEvent::PointerDown {
            x: 53.0,
            y: 5.0,
            button: PointerButton::Primary,
        };
        let pressed = host.input(press).expect("thumb press should be accepted");
        assert!(pressed.handled, "pressing the thumb should start a drag");

        // dragging down by half the track pulls the content with the pointer
        let drag = InputEvent::PointerMove { x: 53.0, y: 20.0 };
        let moved = host.input(drag).expect("thumb drag should be accepted");
        assert!(moved.handled, "dragging should scroll");
        assert!(moved.frame_requested);
        let layout = host
            .browser()
            .layout(host.viewport())
            .expect("dragged page should layout");
        let offset = layout.root.children[0].scroll_offset.1;
        assert!(
            offset > 5.0,
            "a downward drag should scroll down, got {offset}"
        );

        // releasing ends the drag so later moves stop scrolling
        host.input(InputEvent::PointerUp {
            x: 53.0,
            y: 20.0,
            button: PointerButton::Primary,
        })
        .expect("release should be accepted");
        let idle = host
            .input(InputEvent::PointerMove { x: 53.0, y: 40.0 })
            .expect("post-release move should be accepted");
        assert!(!idle.handled, "moving after release should not scroll");
    }

    #[test]
    fn host_routes_wheel_input_to_document_scrolling() {
        let browser = Browser::new(bundle(
            r#"<div id="content" style="width: 40px; height: 180px; margin: 0"></div>"#,
        ));
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 120.0,
                height: 100.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        host.render()
            .expect("initial frame should render")
            .expect("initial frame should be present");
        let scroll = host
            .input(InputEvent::Scroll {
                x: 10.0,
                y: 10.0,
                delta_x: 0.0,
                delta_y: 30.0,
            })
            .expect("document wheel input should be accepted");
        assert!(scroll.handled);
        let layout = host
            .browser()
            .layout(Size {
                width: 120.0,
                height: 100.0,
            })
            .expect("scrolled document should layout");
        assert_eq!(layout.root.children[0].rect.y, -30.0);
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
                br#"self.onmessage = (event) => { postMessage(event.data + "!"); };"#.to_vec(),
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
    fn pointer_drag_selects_control_text_and_the_clipboard_shortcuts_edit_it() {
        use crate::navigation::ClipboardProvider;
        use std::cell::RefCell;

        #[derive(Default)]
        struct RecordingClipboard {
            text: RefCell<String>,
        }

        impl ClipboardProvider for RecordingClipboard {
            fn read_text(&self) -> Result<String, NavigationError> {
                Ok(self.text.borrow().clone())
            }

            fn write_text(&self, text: &str) -> Result<(), NavigationError> {
                *self.text.borrow_mut() = text.to_string();
                Ok(())
            }
        }

        let clipboard = Rc::new(RecordingClipboard::default());
        let browser = Browser::new(bundle(
            "<input id='name' value='hello world' style='width: 300px; margin: 0'>",
        ))
        .with_clipboard_provider(clipboard.clone());
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 320.0,
                height: 60.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");

        // a press near the start puts the caret at the beginning
        assert!(
            host.input(InputEvent::PointerDown {
                x: 3.0,
                y: 10.0,
                button: PointerButton::Primary,
            })
            .expect("pointer down should be accepted")
            .handled
        );
        // dragging to the right edge extends the selection to the end of the value
        assert!(
            host.input(InputEvent::PointerMove { x: 290.0, y: 10.0 })
                .expect("pointer move should be accepted")
                .handled
        );
        host.input(InputEvent::PointerUp {
            x: 290.0,
            y: 10.0,
            button: PointerButton::Primary,
        })
        .expect("pointer up should be accepted");

        let copy = InputEvent::KeyDownWithModifiers {
            key: "c".to_string(),
            modifiers: KeyModifiers {
                control: true,
                ..KeyModifiers::default()
            },
        };
        assert!(host.input(copy).expect("copy should be handled").handled);
        assert_eq!(clipboard.text.borrow().as_str(), "hello world");

        let cut = InputEvent::KeyDownWithModifiers {
            key: "x".to_string(),
            modifiers: KeyModifiers {
                control: true,
                ..KeyModifiers::default()
            },
        };
        assert!(host.input(cut).expect("cut should be handled").handled);
        let document = host.browser().document().expect("document should exist");
        let path = document
            .borrow()
            .find_element_by_id("name")
            .expect("input should exist");
        assert_eq!(
            host.browser().control_value(&path).as_deref(),
            Some(""),
            "cutting the selection empties the control"
        );

        let paste = InputEvent::KeyDownWithModifiers {
            key: "v".to_string(),
            modifiers: KeyModifiers {
                control: true,
                ..KeyModifiers::default()
            },
        };
        assert!(host.input(paste).expect("paste should be handled").handled);
        assert_eq!(
            host.browser().control_value(&path).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn pointer_drag_selects_prose_and_ctrl_c_copies_it() {
        use crate::navigation::ClipboardProvider;

        struct RecordingClipboard {
            text: std::cell::RefCell<String>,
        }

        impl ClipboardProvider for RecordingClipboard {
            fn read_text(&self) -> Result<String, NavigationError> {
                Ok(self.text.borrow().clone())
            }

            fn write_text(&self, text: &str) -> Result<(), NavigationError> {
                *self.text.borrow_mut() = text.to_string();
                Ok(())
            }
        }

        let clipboard = Rc::new(RecordingClipboard {
            text: std::cell::RefCell::new(String::new()),
        });
        let browser = Browser::new(bundle(
            "<style>p { margin: 0; font-size: 16px; }</style><p id='copy'>selectable prose</p>",
        ))
        .with_clipboard_provider(clipboard.clone());
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 320.0,
                height: 80.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        // dragging across the paragraph selects its text
        assert!(
            host.input(InputEvent::PointerDown {
                x: 12.0,
                y: 14.0,
                button: PointerButton::Primary,
            })
            .expect("pointer down should be accepted")
            .handled,
            "pressing on prose should start a selection"
        );
        assert!(
            host.input(InputEvent::PointerMove { x: 300.0, y: 10.0 })
                .expect("pointer move should be accepted")
                .handled,
            "dragging over prose should extend the selection"
        );
        host.input(InputEvent::PointerUp {
            x: 300.0,
            y: 14.0,
            button: PointerButton::Primary,
        })
        .expect("pointer up should be accepted");

        // the selection paints as a highlight behind the glyphs
        let layout = host
            .browser()
            .layout(host.viewport())
            .expect("layout should resolve");
        assert!(
            !layout.prose_selections.is_empty(),
            "the drag should leave a document selection"
        );
        let canvas = layout.paint();
        let pixels = canvas.rgba_pixels();
        let highlight_count = pixels
            .chunks_exact(4)
            .filter(|pixel| pixel[..3] == [170, 205, 238])
            .count();
        assert!(
            highlight_count > 0,
            "the selected span should paint the highlight colour"
        );
        // Ctrl+C copies the prose through the host clipboard seam
        let copy = InputEvent::KeyDownWithModifiers {
            key: "c".to_string(),
            modifiers: KeyModifiers {
                control: true,
                ..KeyModifiers::default()
            },
        };
        assert!(host.input(copy).expect("copy should be handled").handled);
        let copied = clipboard.text.borrow().clone();
        assert!(
            ["selectable", "prose"]
                .iter()
                .any(|word| copied.contains(word)),
            "the copied text should come from the paragraph, got {copied:?}"
        );
    }

    #[test]
    fn a_host_can_show_the_engine_recovery_document_after_a_restart() {
        let mut host = EngineHost::new(
            Browser::new(bundle("<main><p id='status'>ready</p></main>")),
            Size {
                width: 200.0,
                height: 120.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        host.browser_mut()
            .show_recovery(500, "the engine worker stopped");
        let document = host.browser().document().expect("document should exist");
        assert!(
            document.borrow().find_element_by_id("status").is_none(),
            "the recovery document replaces the app document"
        );
        let failure = host
            .last_navigation_failure()
            .expect("a recovery leaves a typed failure");
        assert_eq!(failure.status, 500);
        assert!(failure.detail.contains("worker stopped"));
        let frame = host
            .render()
            .expect("the recovery document should render")
            .expect("a frame should be produced");
        assert!(frame.canvas.non_background_pixels() > 0);
    }

    #[test]
    fn hovering_a_button_transitions_its_colour_over_the_clock() {
        let mut host = EngineHost::new(
            Browser::new(bundle(
                "<style>button { width: 100px; height: 40px; margin: 0; background-color: #000000; transition: background-color 200ms linear } button:hover { background-color: #ffffff }</style><button id='go'>go</button>",
            )),
            Size {
                width: 160.0,
                height: 80.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let background = |host: &EngineHost| {
            let layout = host
                .browser()
                .layout(host.viewport())
                .expect("layout should resolve");
            find_button(&layout.root)
                .expect("the button should lay out")
                .background
        };
        assert_eq!(background(&host), Color::opaque(0, 0, 0));

        // moving onto the button starts the transition rather than jumping to the target
        host.input(InputEvent::PointerMove { x: 50.0, y: 20.0 })
            .expect("pointer move should be accepted");
        let started = background(&host);
        assert!(
            started.red < 40,
            "the transition starts near the old colour, got {started:?}"
        );
        assert!(host.browser().has_active_animations());

        host.input(InputEvent::AdvanceTime(100))
            .expect("the clock should advance");
        let midway = background(&host);
        assert!(
            (100..=160).contains(&midway.red),
            "halfway through 200ms the colour should be halfway, got {midway:?}"
        );

        host.input(InputEvent::AdvanceTime(150))
            .expect("the clock should advance");
        assert_eq!(background(&host), Color::opaque(255, 255, 255));
        assert!(
            !host.browser().has_active_animations(),
            "a finished transition stops asking for frames"
        );
    }

    fn find_button(layout_box: &crate::layout::LayoutBox) -> Option<&crate::layout::LayoutBox> {
        if layout_box.tag == "button" {
            return Some(layout_box);
        }
        layout_box.children.iter().find_map(find_button)
    }

    #[test]
    fn device_scale_only_changes_the_frame_surface() {
        let mut host = EngineHost::with_scale(
            Browser::new(bundle("<main><p id='status'>ready</p></main>")),
            Size {
                width: 160.0,
                height: 100.0,
            },
            2.0,
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let frame = host
            .render()
            .expect("frame should render")
            .expect("first frame should be dirty");
        assert_eq!(frame.scale, 2.0);
        assert_eq!(
            (frame.viewport, frame.canvas.width(), frame.canvas.height()),
            (
                Size {
                    width: 160.0,
                    height: 100.0
                },
                320,
                200
            )
        );

        assert!(
            host.input(InputEvent::ScaleChanged(1.0))
                .expect("scale change should be accepted")
                .frame_requested
        );
        let frame = host
            .render()
            .expect("frame should render")
            .expect("scale change should invalidate the frame");
        assert_eq!(
            (frame.scale, frame.canvas.width(), frame.canvas.height()),
            (1.0, 160, 100)
        );
    }

    #[test]
    fn pages_read_the_host_viewport_and_device_pixel_ratio() {
        let mut host = EngineHost::with_scale(
            Browser::new(bundle(
                r#"<p id="status">idle</p><script>
                    document.getElementById("status").textContent =
                        window.innerWidth + "x" + window.innerHeight + "@" + window.devicePixelRatio;
                </script>"#,
            )),
            Size {
                width: 640.0,
                height: 480.0,
            },
            1.5,
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let document = host.browser().document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("640x480@1.5")
        );
    }

    #[test]
    fn rendered_frames_carry_the_document_title() {
        let mut host = EngineHost::new(
            Browser::new(bundle(
                "<html><head><title>Ceri Console</title></head><body><p>ready</p></body></html>",
            )),
            Size {
                width: 120.0,
                height: 80.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let frame = host
            .render()
            .expect("frame should render")
            .expect("first frame should be dirty");
        assert_eq!(frame.title.as_deref(), Some("Ceri Console"));
        assert_eq!(host.title().as_deref(), Some("Ceri Console"));
    }

    #[test]
    fn host_routes_pointer_input_to_relative_visual_coordinates() {
        let browser = Browser::new(bundle(
            r#"<button id="go" style="position: relative; left: 30px; top: 10px; display: block; width: 80px; height: 24px; margin: 0">run</button><p id="status">idle</p><script>
                document.getElementById("go").addEventListener("click", () => {
                    document.getElementById("status").textContent = "clicked";
                });
            </script>"#,
        ));
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 160.0,
                height: 100.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        assert!(
            host.input(InputEvent::Click { x: 35.0, y: 15.0 })
                .expect("visual click should dispatch")
                .handled
        );
        let document = host.browser().document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("clicked")
        );
    }

    #[test]
    fn host_routes_clicks_through_atomic_stacking_contexts() {
        let browser = Browser::new(bundle(
            r#"<style>
                #low { position: relative; z-index: 1; width: 30px; height: 30px; margin: 0; }
                #low-child { position: absolute; left: 0; top: 0; width: 30px; height: 30px; margin: 0; z-index: 100; }
                #high { position: relative; z-index: 2; top: -30px; width: 30px; height: 30px; margin: 0; }
            </style>
            <div id="low"><div id="low-child">low</div></div>
            <button id="high">high</button>
            <p id="status">idle</p>
            <script>
                document.getElementById("low-child").addEventListener("click", () => {
                    document.getElementById("status").textContent = "low";
                });
                document.getElementById("high").addEventListener("click", () => {
                    document.getElementById("status").textContent = "high";
                });
            </script>"#,
        ));
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 80.0,
                height: 80.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("stacking page should load");
        let click = host
            .input(InputEvent::Click { x: 10.0, y: 10.0 })
            .expect("stacking click should be accepted");
        assert!(click.handled);
        let document = host.browser().document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("high")
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
        assert!(host.click_id("name").expect("click should focus the input"));
        assert!(
            host.input(InputEvent::TextInput {
                text: "ust".to_string(),
            })
            .expect("text should be inserted")
            .handled
        );
        assert!(
            host.input(InputEvent::KeyDown {
                key: "Backspace".to_string(),
            })
            .expect("backspace should be handled")
            .handled
        );
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
    fn host_routes_modifier_key_selection_to_the_focused_control() {
        let browser = Browser::new(bundle(
            r#"<input id="name" value="abcd" style="display: block; width: 180px; height: 32px"><p id="status">idle</p><script>
                const input = document.getElementById("name");
                const status = document.getElementById("status");
                let flags = "";
                input.addEventListener("keydown", (event) => {
                    flags = event.shiftKey + ":" + event.ctrlKey + ":" + event.altKey + ":" + event.metaKey;
                });
                input.addEventListener("select", () => {
                    status.textContent = input.selectionStart + "|" + input.selectionEnd + "|" + input.selectionDirection + "|" + flags;
                });
                input.focus();
                input.setSelectionRange(3, 3);
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
        let document = host.browser().document().expect("document should exist");
        assert!(
            host.input(InputEvent::KeyDownWithModifiers {
                key: "ArrowLeft".to_string(),
                modifiers: KeyModifiers {
                    shift: true,
                    control: true,
                    alt: false,
                    meta: true,
                },
            })
            .expect("shift arrow should be handled")
            .handled
        );
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("2|3|backward|true:true:false:true")
        );
    }

    #[test]
    fn host_routes_native_composition_updates_and_commits() {
        let browser = Browser::new(bundle(
            r#"<input id="name" value="ab" style="display: block; width: 180px; height: 32px"><p id="status">idle</p><script>
                const input = document.getElementById("name");
                const status = document.getElementById("status");
                let events = "";
                input.addEventListener("compositionstart", () => { events = events + "start|"; });
                input.addEventListener("compositionupdate", () => { events = events + "update:" + input.value + "|"; });
                input.addEventListener("input", () => { events = events + "input:" + input.value + "|"; });
                input.addEventListener("compositionend", () => { status.textContent = events + "end:" + input.value; });
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
        assert!(host.click_id("name").expect("click should focus the input"));
        assert!(
            host.input(InputEvent::CompositionUpdate {
                text: "x".to_string(),
                cursor: Some((1, 1)),
            })
            .expect("composition update should be handled")
            .handled
        );
        assert!(
            host.input(InputEvent::CompositionCommit {
                text: "安".to_string(),
            })
            .expect("composition commit should be handled")
            .handled
        );
        let document = host.browser().document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(&status).as_deref(),
            Some("start|update:ab|input:abx|input:ab安|end:ab安")
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
            host.browser()
                .current_url()
                .expect("URL should exist")
                .path(),
            "/about.html"
        );
        assert!(host.back().expect("history back should run"));
        assert_eq!(
            host.browser()
                .current_url()
                .expect("URL should exist")
                .path(),
            "/"
        );
        assert!(
            host.render()
                .expect("history frame should render")
                .is_some()
        );
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
                assert_eq!(
                    result.expect("navigation should load"),
                    NavigationStatus::Loaded
                );
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
