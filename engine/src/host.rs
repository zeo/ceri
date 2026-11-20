use crate::AccessibilityNode;
use crate::bridge::Waker;
use crate::layout::Size;
use crate::navigation::{
    Browser, NavigationError, NavigationFailure, NavigationStatus, UserPreferences,
};
use crate::paint::Canvas;
pub use crate::script::KeyModifiers;
use crate::script::{Incoming, PauseSource};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

mod automation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    Primary,
    Secondary,
    Auxiliary,
}

// the MouseEvent.button number of a host button
fn button_index(button: PointerButton) -> i16 {
    match button {
        PointerButton::Primary => 0,
        PointerButton::Auxiliary => 1,
        PointerButton::Secondary => 2,
    }
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
    // a key press as a window reports it, with the physical key code and auto-repeat flag
    Key {
        key: String,
        code: String,
        modifiers: KeyModifiers,
        repeat: bool,
        pressed: bool,
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
    // the pointer shape after a pointer move, for the host to show
    pub cursor: Option<crate::css::Cursor>,
    // a press on an app-region drag area, which the window answers instead of the page
    pub region: Option<RegionPress>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionPress {
    Move,
    ToggleMaximize,
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub sequence: u64,
    pub viewport: Size,
    pub scale: f32,
    pub title: Option<String>,
    pub canvas: std::sync::Arc<Canvas>,
    // video frames to show under the canvas where it leaves them transparent
    #[cfg_attr(
        not(all(feature = "windowed", target_os = "windows")),
        expect(dead_code)
    )]
    pub(crate) planes: Vec<crate::paint::PlaneFrame>,
}

pub struct EngineHost {
    browser: Browser,
    viewport: Size,
    scale: f32,
    selecting: bool,
    selecting_thumb: Option<crate::ThumbDrag>,
    range_drag: Option<crate::layout::RangeDrag>,
    next_frame: u64,
    dirty: bool,
    // milliseconds of host clock, used to count rapid presses as one series
    clock: u64,
    last_press: Option<Press>,
    // the press in progress went to a drag region, so its release and click stay out of the page
    region_press: bool,
    // where a primary press on a draggable element began, until it moves far enough to drag
    drag_press: Option<(f32, f32, crate::dom::NodeId)>,
    // the frames go to a presenter that can show videos under the canvas
    planes: bool,
}

#[derive(Clone, Copy)]
struct Press {
    time: u64,
    x: f32,
    y: f32,
    count: u32,
}

// how far a press moves before it drags, in CSS pixels along either axis, as in Blink
#[cfg(target_os = "macos")]
const DRAG_THRESHOLD: f32 = 3.0;
#[cfg(not(target_os = "macos"))]
const DRAG_THRESHOLD: f32 = 4.0;

// the platform defaults on Windows; a press inside both continues the series
const MULTI_CLICK_MILLISECONDS: u64 = 500;
const MULTI_CLICK_DISTANCE: f32 = 4.0;

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
    // a live worker's page changed on its own and has a new frame to render
    Invalidated,
    // a live worker's own clock failed
    Failed(NavigationError),
}

pub(crate) enum EngineWorkerCommand {
    Navigate { request_id: u64, url: String },
    Input { request_id: u64, event: InputEvent },
    Render { request_id: u64 },
    // the frames go to a presenter that shows videos under the canvas
    Planes,
    Accessibility { request_id: u64 },
    NavigationFailure { request_id: u64 },
    // work arrived from another thread
    Wake,
    // an event for the page's ceri.listen callbacks
    HostEvent { name: String, payload: String },
    Inspector(crate::devtools::Inbound),
    Shutdown,
}

pub struct EngineWorker {
    commands: SyncSender<EngineWorkerCommand>,
    events: Receiver<EngineWorkerEvent>,
    next_request_id: u64,
    // the window's entry in the devtools target list, while devtools are served
    _devtools: Option<crate::devtools::Registration>,
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
                _devtools: None,
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err("engine worker stopped during initialization".to_string()),
        }
    }

    // a worker whose page keeps real time, waking itself for timers and animation, and
    // calling notify whenever it has an event to poll
    pub fn spawn_live<F>(viewport: Size, factory: F, notify: Waker) -> Result<Self, String>
    where
        F: FnOnce() -> Result<Browser, String> + Send + 'static,
    {
        let (commands, command_receiver) = mpsc::sync_channel(64);
        let (events, event_receiver) = mpsc::channel();
        let (ready, ready_receiver) = mpsc::channel();
        let wake = commands.clone();
        let devtools = crate::devtools::register(commands.clone());
        let page = devtools
            .as_ref()
            .map(|registration| registration.page.clone());
        let target = devtools
            .as_ref()
            .map(|registration| registration.id.clone());
        thread::Builder::new()
            .name("ceri-engine".to_string())
            .spawn(move || {
                if let Some(target) = target {
                    crate::script::set_frame_id(&target);
                }
                let mut browser = match factory() {
                    Ok(browser) => browser,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                // a full queue already holds a command that will wake the worker
                browser.set_waker(Arc::new(move || {
                    let _ = wake.try_send(EngineWorkerCommand::Wake);
                }));
                let _ = ready.send(Ok(()));
                run_live_engine_worker(
                    EngineHost::new(browser, viewport),
                    command_receiver,
                    events,
                    notify,
                    page,
                );
            })
            .map_err(|error| format!("could not start engine worker: {error}"))?;
        match ready_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                commands,
                events: event_receiver,
                next_request_id: 1,
                _devtools: devtools,
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

    // an event for the page's ceri.listen callbacks
    pub fn emit(&mut self, name: &str, payload: &str) -> Result<(), String> {
        self.queue(EngineWorkerCommand::HostEvent {
            name: name.to_string(),
            payload: payload.to_string(),
        })
    }

    pub fn render(&mut self) -> Result<u64, String> {
        let request_id = self.next_request();
        self.queue(EngineWorkerCommand::Render { request_id })?;
        Ok(request_id)
    }

    pub(crate) fn show_planes(&mut self) -> Result<(), String> {
        self.queue(EngineWorkerCommand::Planes)
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
        let Some(event) = answer(&mut host, command) else {
            break;
        };
        if events.send(event).is_err() {
            break;
        }
    }
}

// a worker that keeps its page's clock in real time and sleeps until the page, a command or
// a waker has work for it
fn run_live_engine_worker(
    mut host: EngineHost,
    commands: Receiver<EngineWorkerCommand>,
    events: Sender<EngineWorkerEvent>,
    notify: Waker,
    page: Option<Arc<Mutex<crate::devtools::PageInfo>>>,
) {
    let commands = Rc::new(commands);
    let queued = Rc::new(RefCell::new(VecDeque::new()));
    host.browser.set_pause_source(Rc::new(PausedQueue {
        commands: commands.clone(),
        queued: queued.clone(),
    }));
    let send = |event| {
        let sent = events.send(event).is_ok();
        notify();
        sent
    };
    let mut last_tick = Instant::now();
    let mut announced = false;
    let mut held = None;
    let frame = Duration::from_millis(crate::navigation::FRAME_INTERVAL);
    let mut last_frame: Option<Instant> = None;
    // a render asked for before its frame slot opens
    let mut deferred = None;
    let mut fine_timer = false;
    loop {
        // timers, frame callbacks, css animations and the window asking again as each frame
        // lands all tick apart, so a page renders at most once per frame slot
        let slot = (deferred.is_some() || (host.is_dirty() && !announced))
            .then(|| last_frame.map_or(Duration::ZERO, |at| frame.saturating_sub(at.elapsed())));
        let wake = host
            .next_wake()
            .map(|delay| Duration::from_millis(delay.max(1)));
        let wait = wake.into_iter().chain(slot).min();
        let animating = wait.is_some_and(|wait| wait < frame * 2);
        hold_fine_timer(&mut fine_timer, animating);
        if !animating {
            host.browser.rest();
        }
        let command = match held.take().or_else(|| queued.borrow_mut().pop_front()) {
            Some(command) => Some(command),
            None => match wait {
                Some(delay) => match commands.recv_timeout(delay) {
                    Ok(command) => Some(command),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                },
                None => match commands.recv() {
                    Ok(command) => Some(command),
                    Err(_) => break,
                },
            },
        };
        // only the newest of a run of queued pointer moves matters, so a fast mouse cannot
        // queue more work than one frame needs
        let command = match command {
            Some(mut command) if is_pointer_move(&command) => {
                while let Ok(next) = commands.try_recv() {
                    if !is_pointer_move(&next) {
                        held = Some(next);
                        break;
                    }
                    command = next;
                }
                Some(command)
            }
            other => other,
        };
        let elapsed = last_tick.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        last_tick += Duration::from_millis(elapsed);
        // the clock starts with the first page
        if host.browser.current_url().is_some()
            && let Err(error) = host.input(InputEvent::AdvanceTime(elapsed))
        {
            send(EngineWorkerEvent::Failed(error));
            break;
        }
        let slot_open = last_frame.is_none_or(|at| at.elapsed() >= frame);
        let command = match command {
            Some(EngineWorkerCommand::Render { request_id })
                if !slot_open && deferred.is_none() =>
            {
                deferred = Some(request_id);
                None
            }
            None if slot_open => deferred
                .take()
                .map(|request_id| EngineWorkerCommand::Render { request_id }),
            other => {
                if slot_open
                    && held.is_none()
                    && let Some(request_id) = deferred.take()
                {
                    held = Some(EngineWorkerCommand::Render { request_id });
                }
                other
            }
        };
        let started = Instant::now();
        let event = match command {
            Some(EngineWorkerCommand::Shutdown) => break,
            Some(command) => answer(&mut host, command),
            None => None,
        };
        host.browser.flush_inspector();
        if let Some(event) = event {
            if let Some(page) = &page
                && matches!(
                    event,
                    EngineWorkerEvent::Navigated { .. } | EngineWorkerEvent::Frame { .. }
                )
            {
                let mut page = page.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                page.title = host.title().unwrap_or_default();
                page.url = host
                    .browser
                    .current_url()
                    .map(ToString::to_string)
                    .unwrap_or_default();
            }
            if let EngineWorkerEvent::Frame { result, .. } = &event {
                announced = false;
                if matches!(result, Ok(Some(_))) {
                    last_frame = Some(started);
                }
            }
            if !send(event) {
                break;
            }
        }
        if host.is_dirty() && !announced && last_frame.is_none_or(|at| at.elapsed() >= frame) {
            announced = true;
            if !send(EngineWorkerEvent::Invalidated) {
                break;
            }
        }
    }
    hold_fine_timer(&mut fine_timer, false);
}

// windows rounds channel timeouts up to its default 15.6 ms tick, which holds animations near
// 40 fps, so the 1 ms tick is held only while a wait that short is pending
#[cfg(all(target_os = "windows", feature = "windowed"))]
fn hold_fine_timer(held: &mut bool, wanted: bool) {
    use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
    if *held != wanted {
        // SAFETY: `held` pairs every begin with one end
        unsafe {
            if wanted {
                timeBeginPeriod(1);
            } else {
                timeEndPeriod(1);
            }
        }
        *held = wanted;
    }
}

#[cfg(not(all(target_os = "windows", feature = "windowed")))]
fn hold_fine_timer(_: &mut bool, _: bool) {}

// the worker's queue as a page paused in its debugger reads it: devtools messages go to the
// debugger and the rest waits in order for the page to run again
struct PausedQueue {
    commands: Rc<Receiver<EngineWorkerCommand>>,
    queued: Rc<RefCell<VecDeque<EngineWorkerCommand>>>,
}

impl PausedQueue {
    fn sort(&self, command: EngineWorkerCommand) -> Option<Incoming> {
        use crate::devtools::Inbound;
        let command = match command {
            EngineWorkerCommand::Inspector(Inbound::Message { session, text }) => {
                return Some(Incoming::Message { session, text });
            }
            command => command,
        };
        let incoming = match &command {
            EngineWorkerCommand::Inspector(Inbound::Detach { session }) => {
                Some(Incoming::Detached(*session))
            }
            EngineWorkerCommand::Shutdown => Some(Incoming::Closed),
            _ => None,
        };
        let mut queued = self.queued.borrow_mut();
        match queued.back_mut() {
            Some(last) if is_pointer_move(last) && is_pointer_move(&command) => *last = command,
            _ => queued.push_back(command),
        }
        incoming
    }
}

impl PauseSource for PausedQueue {
    fn next(&self) -> Incoming {
        loop {
            let Ok(command) = self.commands.recv() else {
                return Incoming::Closed;
            };
            if let Some(incoming) = self.sort(command) {
                return incoming;
            }
        }
    }

    fn poll(&self) -> Option<Incoming> {
        while let Ok(command) = self.commands.try_recv() {
            if let Some(incoming) = self.sort(command) {
                return Some(incoming);
            }
        }
        None
    }

    fn defer(&self, session: u64, text: String) {
        let message = crate::devtools::Inbound::Message { session, text };
        self.queued
            .borrow_mut()
            .push_back(EngineWorkerCommand::Inspector(message));
    }
}

fn is_pointer_move(command: &EngineWorkerCommand) -> bool {
    matches!(
        command,
        EngineWorkerCommand::Input {
            event: InputEvent::PointerMove { .. },
            ..
        }
    )
}

fn answer(host: &mut EngineHost, command: EngineWorkerCommand) -> Option<EngineWorkerEvent> {
    Some(match command {
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
        EngineWorkerCommand::Planes => {
            host.planes = true;
            host.dirty = true;
            return None;
        }
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
        // the clock tick before every command already pumped the page
        EngineWorkerCommand::Wake => return None,
        EngineWorkerCommand::HostEvent { name, payload } => {
            return match host.emit_event(&name, &payload) {
                Ok(()) => None,
                Err(error) => Some(EngineWorkerEvent::Failed(error)),
            };
        }
        EngineWorkerCommand::Inspector(inbound) => {
            use crate::devtools::Inbound;
            match inbound {
                Inbound::Attach { session, outgoing } => {
                    host.browser.attach_inspector(session, outgoing)
                }
                Inbound::Detach { session } => host.browser.detach_inspector(session),
                Inbound::Message { session, text } => {
                    // whatever the session ran may have changed the page
                    host.dirty = true;
                    if let Some((id, answer)) = host.automation_call(&text) {
                        host.browser.answer_inspector(session, id, answer);
                        return None;
                    }
                    if let Some(url) = host.browser.inspect(session, &text) {
                        return Some(EngineWorkerEvent::Navigated {
                            request_id: 0,
                            result: host.navigate(&url),
                        });
                    }
                }
            }
            return None;
        }
        EngineWorkerCommand::Shutdown => return None,
    })
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
            range_drag: None,
            next_frame: 1,
            dirty: true,
            clock: 0,
            last_press: None,
            region_press: false,
            drag_press: None,
            planes: false,
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

    fn count_press(&mut self, x: f32, y: f32) -> u32 {
        let count = match self.last_press {
            Some(press)
                if self.clock.saturating_sub(press.time) <= MULTI_CLICK_MILLISECONDS
                    && (press.x - x).abs() <= MULTI_CLICK_DISTANCE
                    && (press.y - y).abs() <= MULTI_CLICK_DISTANCE =>
            {
                press.count + 1
            }
            _ => 1,
        };
        self.last_press = Some(Press {
            time: self.clock,
            x,
            y,
            count,
        });
        count
    }

    pub fn input(&mut self, event: InputEvent) -> Result<InputResult, NavigationError> {
        self.pump_workers()?;
        // a devtools session picking a node takes the pointer from the page
        let picked = match &event {
            InputEvent::PointerMove { x, y } => self.browser.pick_hover(self.viewport, *x, *y)?,
            InputEvent::PointerDown {
                x,
                y,
                button: PointerButton::Primary,
            } => self.browser.pick_press(self.viewport, *x, *y)?,
            InputEvent::PointerUp {
                button: PointerButton::Primary,
                ..
            } => self.browser.pick_release(false),
            InputEvent::Click { .. } => self.browser.pick_release(true),
            _ => false,
        };
        let result = match event {
            _ if picked => InputResult {
                handled: true,
                frame_requested: true,
                ..InputResult::default()
            },
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
                let started = match self.drag_press {
                    Some((from_x, from_y, source))
                        if (x - from_x).abs() >= DRAG_THRESHOLD
                            || (y - from_y).abs() >= DRAG_THRESHOLD =>
                    {
                        self.drag_press = None;
                        self.selecting = false;
                        self.browser.start_drag(source, from_x, from_y)?
                    }
                    _ => false,
                };
                let dragged = match (&self.selecting_thumb, &self.range_drag) {
                    (Some(drag), _) => self.browser.drag_thumb(drag, x, y)?,
                    (_, Some(drag)) => self.browser.drag_range(drag, x)?,
                    _ => false,
                };
                let extended =
                    self.selecting && self.browser.select_at(self.viewport, x, y, true)?;
                InputResult {
                    handled: dispatched || started || dragged || extended,
                    frame_requested: dragged || extended,
                    cursor: Some(self.browser.cursor()),
                    ..InputResult::default()
                }
            }
            InputEvent::PointerDown { x, y, button }
                if button == PointerButton::Primary
                    && self.browser.drag_region_at(self.viewport, x, y) =>
            {
                self.region_press = true;
                let region = if self.count_press(x, y) >= 2 {
                    self.last_press = None;
                    RegionPress::ToggleMaximize
                } else {
                    RegionPress::Move
                };
                InputResult {
                    region: Some(region),
                    ..InputResult::default()
                }
            }
            InputEvent::PointerUp { .. } | InputEvent::Click { .. } if self.region_press => {
                InputResult::default()
            }
            InputEvent::PointerDown { x, y, button } => {
                self.region_press = false;
                self.browser.set_pointer_button(button_index(button), true);
                let dispatched = self
                    .browser
                    .dispatch_at(self.viewport, x, y, "pointerdown")?;
                let primary = button == PointerButton::Primary;
                let count = if primary { self.count_press(x, y) } else { 1 };
                let thumb_drag = primary
                    .then(|| self.browser.thumb_at(self.viewport, x, y))
                    .flatten();
                match &thumb_drag {
                    Some(drag) if drag.page != 0.0 => {
                        self.browser.page_by_bar(drag)?;
                    }
                    Some(drag) => self.selecting_thumb = Some(drag.clone()),
                    None => {}
                }
                if primary && thumb_drag.is_none() {
                    self.range_drag = self.browser.press_range(self.viewport, x, y)?;
                }
                let grabbed = thumb_drag.is_some() || self.range_drag.is_some();
                self.drag_press = (primary && !grabbed)
                    .then(|| self.browser.drag_source_at(self.viewport, x, y))
                    .flatten()
                    .map(|source| (x, y, source));
                // a press on a scrollbar thumb or a slider drags it and leaves the selection alone
                let selected = primary
                    && !grabbed
                    && if count >= 2 {
                        self.browser
                            .select_unit_at(self.viewport, x, y, count.min(3))?
                    } else {
                        self.browser.select_at(self.viewport, x, y, false)?
                    };
                self.selecting = selected;
                InputResult {
                    handled: dispatched || grabbed || selected,
                    frame_requested: grabbed || selected,
                    ..InputResult::default()
                }
            }
            InputEvent::PointerUp { x, y, button } => {
                self.browser.set_pointer_button(button_index(button), false);
                if button == PointerButton::Primary {
                    self.drag_press = None;
                }
                let released = if button == PointerButton::Primary {
                    self.selecting = false;
                    self.selecting_thumb = None;
                    self.range_drag.take()
                } else {
                    None
                };
                let handled = self.browser.dispatch_at(self.viewport, x, y, "pointerup")?;
                if let Some(drag) = &released {
                    self.browser.release_range(drag)?;
                }
                InputResult {
                    handled: handled || released.is_some(),
                    frame_requested: released.is_some(),
                    ..InputResult::default()
                }
            }
            InputEvent::Click { x, y } => {
                let count = self.last_press.map_or(1, |press| press.count);
                InputResult {
                    handled: self
                        .browser
                        .click_at_with_count(self.viewport, x, y, count)?,
                    ..InputResult::default()
                }
            }
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
            InputEvent::Key {
                key,
                code,
                modifiers,
                repeat,
                pressed,
            } => InputResult {
                handled: if pressed {
                    self.browser
                        .key_down_with_code(&key, Some(&code), modifiers, repeat)?
                } else {
                    self.browser.key_up(&key, &code, modifiers)?
                },
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
                self.clock = self.clock.saturating_add(milliseconds);
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
        let (canvas, planes) = self
            .browser
            .paint_planes(self.viewport, self.scale, self.planes)?;
        let frame = Frame {
            sequence: self.next_frame,
            viewport: self.viewport,
            scale: self.scale,
            title: self.browser.title(),
            canvas,
            planes,
        };
        self.next_frame = self.next_frame.saturating_add(1);
        self.dirty = false;
        Ok(Some(frame))
    }

    // milliseconds until the page has work of its own
    pub fn next_wake(&self) -> Option<u64> {
        self.browser.current_url()?;
        self.browser.next_wake()
    }

    // an event for the page's ceri.listen callbacks; a page with no listeners ignores it
    pub fn emit_event(&mut self, name: &str, payload: &str) -> Result<(), NavigationError> {
        if self.browser.current_url().is_none() {
            return Ok(());
        }
        self.dirty |= self.browser.emit_event(name, payload)? > 0;
        Ok(())
    }

    // whether a render would produce a new frame
    pub fn is_dirty(&self) -> bool {
        self.dirty
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
    use serde_json::{Value as Json, json};
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};
    use std::thread::ThreadId;

    // the pages these tests build lay out from the viewport's corner, as most apps reset it
    const PAGE_RESET: &str = "<style>body { margin: 0 }</style>";

    fn bundle(source: &str) -> ResourceBundle {
        let mut resources = ResourceBundle::default();
        resources
            .insert(
                "/index.html",
                "text/html; charset=utf-8",
                format!("{PAGE_RESET}{source}").into_bytes(),
            )
            .expect("HTML resource should be accepted");
        resources
    }

    #[test]
    fn live_workers_wake_for_timers_and_command_replies_and_sleep_when_idle() {
        use crate::bridge::{Capabilities, CommandCall, CommandReply, CommandRouter, Grant};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Duration, Instant};

        struct Later;

        impl CommandRouter for Later {
            fn invoke(&self, _call: CommandCall, reply: CommandReply) {
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(30));
                    reply.send(Ok("\"late\"".to_string()));
                });
            }
        }

        let notified = Arc::new(AtomicUsize::new(0));
        let counter = notified.clone();
        let mut worker = EngineWorker::spawn_live(
            Size {
                width: 120.0,
                height: 80.0,
            },
            || {
                Ok(Browser::new(bundle(
                    r#"<p id="status">idle</p><script>
                        const status = document.getElementById("status");
                        setTimeout(() => { status.textContent = "timer"; }, 40);
                        setTimeout(() => {
                            window.ceri.invoke("later").then((reply) => { status.textContent = reply; });
                        }, 1000);
                    </script>"#,
                ))
                .with_host_bridge(crate::script::HostBridge::new(
                    Rc::new(Later),
                    Capabilities::new(vec![Grant {
                        windows: vec!["*".to_string()],
                        origins: vec!["*".to_string()],
                        commands: vec!["later".to_string()],
                    }]),
                    "main",
                )))
            },
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .expect("worker should start");
        worker
            .navigate("https://app.ceri.invalid/")
            .expect("navigation should queue");
        let deadline = Instant::now() + Duration::from_secs(5);
        let wait_for = |worker: &EngineWorker, want: &dyn Fn(&EngineWorkerEvent) -> bool| loop {
            assert!(
                Instant::now() < deadline,
                "the worker should report in time"
            );
            if worker.poll().iter().any(want) {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        };
        wait_for(&worker, &|event| {
            matches!(event, EngineWorkerEvent::Navigated { .. })
        });
        worker.render().expect("render should queue");
        // the timer fires on the worker's own clock, with nothing sent to it
        wait_for(&worker, &|event| {
            matches!(event, EngineWorkerEvent::Invalidated)
        });
        worker.render().expect("render should queue");
        wait_for(&worker, &|event| {
            matches!(event, EngineWorkerEvent::Frame { .. })
        });
        thread::sleep(Duration::from_millis(100));
        worker.poll();
        let quiet = notified.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(200));
        assert_eq!(
            notified.load(Ordering::SeqCst),
            quiet,
            "an idle page does not wake anyone"
        );
        // the reply lands from another thread and wakes the worker to settle it
        let says_late = |node: &AccessibilityNode| {
            fn find(node: &AccessibilityNode) -> bool {
                node.name == "late" || node.children.iter().any(find)
            }
            find(node)
        };
        loop {
            assert!(Instant::now() < deadline, "the reply should settle");
            worker.accessibility_tree().expect("tree should queue");
            let settled = loop {
                if let EngineWorkerEvent::Accessibility { tree, .. } =
                    worker.wait().expect("the worker answers")
                {
                    break tree.as_ref().is_some_and(says_late);
                }
            };
            if settled {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        worker.shutdown();
    }

    #[test]
    fn live_workers_answer_a_run_of_queued_pointer_moves_with_the_newest() {
        use std::time::{Duration, Instant};

        let mut worker = EngineWorker::spawn_live(
            Size {
                width: 200.0,
                height: 100.0,
            },
            || {
                Ok(Browser::new(bundle(
                    r#"<main style="height: 100px"><p id="status">none</p></main><script>
                        let moves = 0;
                        let x = 0;
                        // keeps the worker busy while the test queues its moves
                        for (let i = 0; i < 300000; i++) {}
                        document.addEventListener("pointermove", (event) => {
                            moves += 1;
                            x = event.clientX;
                        });
                        document.addEventListener("click", () => {
                            document.getElementById("status").textContent = `moves ${moves} x ${x}`;
                        });
                    </script>"#,
                )))
            },
            Arc::new(|| {}),
        )
        .expect("worker should start");
        worker
            .navigate("https://app.ceri.invalid/")
            .expect("navigation should queue");
        for x in 1..=40 {
            worker
                .input(InputEvent::PointerMove {
                    x: x as f32,
                    y: 20.0,
                })
                .expect("move should queue");
        }
        worker
            .input(InputEvent::Click { x: 40.0, y: 20.0 })
            .expect("click should queue");
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            assert!(Instant::now() < deadline, "the click should land");
            worker.accessibility_tree().expect("tree should queue");
            let tree = loop {
                if let EngineWorkerEvent::Accessibility { tree, .. } =
                    worker.wait().expect("the worker answers")
                {
                    break tree;
                }
            };
            fn find(node: &AccessibilityNode) -> Option<String> {
                if node.name.starts_with("moves ") {
                    return Some(node.name.clone());
                }
                node.children.iter().find_map(find)
            }
            if let Some(status) = tree.as_ref().and_then(find) {
                break status;
            }
            thread::sleep(Duration::from_millis(10));
        };
        let moves: u32 = status
            .split(' ')
            .nth(1)
            .and_then(|count| count.parse().ok())
            .expect("status should count moves");
        assert!(status.ends_with(" x 40"), "the newest move wins: {status}");
        assert!(moves < 40, "queued moves should merge: {status}");
        worker.shutdown();
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
        assert_eq!(layout.body().children[0].children[0].rect.y, -30.0);
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
        let sticky = &layout.body().children[0].children[0];
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
            document.borrow().element_text_content(status).as_deref(),
            Some("clicked")
        );
    }

    #[test]
    fn host_answers_presses_on_drag_regions_instead_of_the_page() {
        let browser = Browser::new(bundle(
            r#"<style>
                body { margin: 0; }
                #bar { app-region: drag; height: 20px; }
                #close { -webkit-app-region: no-drag; width: 20px; height: 20px; margin-left: auto; }
            </style>
            <div id="bar"><div id="close"></div></div>
            <p id="status">idle</p>
            <script>
                const status = document.getElementById("status");
                document.getElementById("bar").addEventListener("pointerdown", () => {
                    status.textContent = "bar";
                });
                document.getElementById("close").addEventListener("click", () => {
                    status.textContent = "close";
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
            .expect("drag region page should load");
        let status = |host: &EngineHost| {
            let document = host.browser().document().expect("document should exist");
            let document = document.borrow();
            let status = document
                .find_element_by_id("status")
                .expect("status should exist");
            document.element_text_content(status)
        };
        let press = |host: &mut EngineHost, x: f32| {
            host.input(InputEvent::PointerDown {
                x,
                y: 10.0,
                button: PointerButton::Primary,
            })
            .expect("press should be accepted")
            .region
        };
        let release = |host: &mut EngineHost, x: f32| {
            host.input(InputEvent::PointerUp {
                x,
                y: 10.0,
                button: PointerButton::Primary,
            })
            .expect("release should be accepted");
            host.input(InputEvent::Click { x, y: 10.0 })
                .expect("click should be accepted");
        };

        assert_eq!(press(&mut host, 10.0), Some(RegionPress::Move));
        release(&mut host, 10.0);
        assert_eq!(status(&host).as_deref(), Some("idle"));
        assert_eq!(press(&mut host, 10.0), Some(RegionPress::ToggleMaximize));
        release(&mut host, 10.0);
        assert_eq!(press(&mut host, 110.0), None);
        release(&mut host, 110.0);
        assert_eq!(status(&host).as_deref(), Some("close"));
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
            document.borrow().element_text_content(status).as_deref(),
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
        assert_eq!(layout.body().children[0].scroll_offset, (0.0, 10.0));
    }

    #[test]
    fn a_slider_follows_the_pointer_and_the_keys_and_reports_input_then_change() {
        let mut host = EngineHost::new(
            Browser::new(bundle(
                r#"<input type="range" id="slider" style="width: 116px; margin: 0">
                <p id="log"></p><p id="model"></p><script>
                    const slider = document.getElementById("slider");
                    const log = document.getElementById("log");
                    slider.addEventListener("input", () => { log.textContent += "i" + slider.value + " "; });
                    slider.addEventListener("change", () => { log.textContent += "c" + slider.value + " "; });
                    const probe = document.createElement("input");
                    probe.type = "range";
                    probe.step = "5";
                    probe.value = "33.4";
                    const snapped = probe.value;
                    probe.valueAsNumber = 71;
                    document.getElementById("model").textContent =
                        snapped + " " + probe.value + " " + probe.valueAsNumber + " " + slider.valueAsNumber;
                </script>"#,
            )),
            Size {
                width: 200.0,
                height: 120.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("slider page should load");
        let text = |host: &EngineHost, id: &str| {
            let document = host.browser().document().expect("document should exist");
            let node = document
                .borrow()
                .find_element_by_id(id)
                .expect("element should exist");
            document
                .borrow()
                .element_text_content(node)
                .unwrap_or_default()
        };
        assert_eq!(text(&host, "model"), "35 70 70 50");
        let press = |x: f32| InputEvent::PointerDown {
            x,
            y: 8.0,
            button: PointerButton::Primary,
        };
        let release = |x: f32| InputEvent::PointerUp {
            x,
            y: 8.0,
            button: PointerButton::Primary,
        };

        // the thumb's center travels from x 8 to 108, so a press on the track at 83 jumps to 75
        assert!(host.input(press(83.0)).expect("press").frame_requested);
        host.input(InputEvent::PointerMove { x: 33.0, y: 40.0 })
            .expect("drag");
        host.input(release(33.0)).expect("release");
        assert_eq!(text(&host, "log"), "i75 i25 c25 ");

        // a press on the thumb keeps the pointer's offset in it instead of jumping
        host.input(press(30.0)).expect("thumb press");
        host.input(InputEvent::PointerMove { x: 50.0, y: 8.0 })
            .expect("thumb drag");
        host.input(release(50.0)).expect("thumb release");
        assert_eq!(text(&host, "log"), "i75 i25 c25 i45 c45 ");

        // the press focused the slider, so the keys step it
        for key in ["ArrowRight", "PageDown", "End", "End"] {
            host.input(InputEvent::KeyDown {
                key: key.to_string(),
            })
            .expect("key should be accepted");
        }
        assert_eq!(
            text(&host, "log"),
            "i75 i25 c25 i45 c45 i46 c46 i36 c36 i100 c100 "
        );
        let layout = host
            .browser()
            .layout(host.viewport())
            .expect("slider page should layout");
        assert!(matches!(
            layout.body().children[0].control,
            Some(crate::layout::ControlPaint::Range { fraction, .. }) if fraction == 1.0
        ));
    }

    #[test]
    fn dragging_the_scrollbar_thumb_scrolls_and_the_track_and_buttons_page() {
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
        let offset = |host: &EngineHost| {
            host.browser()
                .layout(host.viewport())
                .expect("page should layout")
                .body()
                .children[0]
                .scroll_offset
                .1
        };
        let press = |host: &mut EngineHost, y: f32| {
            let pressed = host
                .input(InputEvent::PointerDown {
                    x: 52.0,
                    y,
                    button: PointerButton::Primary,
                })
                .expect("bar press should be accepted");
            assert!(pressed.handled && pressed.frame_requested);
            host.input(InputEvent::PointerUp {
                x: 52.0,
                y,
                button: PointerButton::Primary,
            })
            .expect("release should be accepted");
        };

        // the 15px bar has a 15px arrow button at each end, and the 8px thumb starts 3px
        // below the top one, at y 18, sliding 16px for 240px of scrolling
        host.input(InputEvent::PointerDown {
            x: 52.0,
            y: 20.0,
            button: PointerButton::Primary,
        })
        .expect("thumb press should be accepted");
        let moved = host
            .input(InputEvent::PointerMove { x: 52.0, y: 30.0 })
            .expect("thumb drag should be accepted");
        assert!(moved.handled && moved.frame_requested);
        assert_eq!(offset(&host), 150.0);
        host.input(InputEvent::PointerUp {
            x: 52.0,
            y: 30.0,
            button: PointerButton::Primary,
        })
        .expect("release should be accepted");
        host.input(InputEvent::PointerMove { x: 52.0, y: 40.0 })
            .expect("post-release move should be accepted");
        assert_eq!(offset(&host), 150.0, "a move after release does not drag");

        // the track below the thumb pages by 0.875 of the 60px view, the down arrow by a line
        press(&mut host, 40.0);
        assert_eq!(offset(&host), 202.5);
        press(&mut host, 55.0);
        assert_eq!(offset(&host), 240.0);
        press(&mut host, 22.0);
        assert_eq!(offset(&host), 187.5);
        press(&mut host, 5.0);
        assert_eq!(offset(&host), 147.5);
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
        assert_eq!(layout.body().children[0].rect.y, -30.0);
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
        // the worker runs on its own thread, which a loaded machine may start late
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let _ = host.render().expect("worker frame should render");
            let document = host.browser().document().expect("document should exist");
            let status = document
                .borrow()
                .find_element_by_id("status")
                .expect("status should exist");
            if document.borrow().element_text_content(status).as_deref() == Some("ready!") {
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
                .element_text_content(status)
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
            host.browser().control_value(path).as_deref(),
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
            host.browser().control_value(path).as_deref(),
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
            .as_chunks::<4>()
            .0
            .iter()
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
            document.borrow().element_text_content(status).as_deref(),
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
            document.borrow().element_text_content(status).as_deref(),
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
            document.borrow().element_text_content(status).as_deref(),
            Some("high")
        );
    }

    #[test]
    fn host_pointer_and_key_input_reach_page_listeners_with_details() {
        let browser = Browser::new(bundle(
            r#"<style>body { margin: 0 } button { display: block; width: 100px; height: 40px }</style><button id="go">Go</button><p id="status">idle</p><script>
                const button = document.getElementById("go");
                const seen = [];
                const report = () => document.getElementById("status").textContent = seen.join(",");
                button.addEventListener("mouseenter", () => { seen.push("enter"); report(); });
                button.addEventListener("pointerdown", (event) => { seen.push("down:" + event.button + ":" + event.buttons + ":" + event.clientX); report(); });
                button.addEventListener("mousedown", () => { seen.push("mousedown"); report(); });
                button.addEventListener("pointerup", (event) => { seen.push("up:" + event.buttons); report(); });
                button.addEventListener("click", (event) => { seen.push("click:" + event.clientY); report(); });
                button.addEventListener("keydown", (event) => { seen.push(event.key + ":" + event.code + ":" + event.repeat); report(); });
                button.addEventListener("keyup", (event) => { seen.push("keyup:" + event.code); report(); });
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
        let point = (20.0, 10.0);
        host.input(InputEvent::PointerMove {
            x: point.0,
            y: point.1,
        })
        .expect("move should be accepted");
        press(&mut host, point);
        release(&mut host, point);
        for (pressed, repeat) in [(true, false), (true, true), (false, false)] {
            host.input(InputEvent::Key {
                key: "a".to_string(),
                code: "KeyA".to_string(),
                modifiers: KeyModifiers::default(),
                repeat,
                pressed,
            })
            .expect("key should be accepted");
        }
        let document = host.browser().document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("enter,down:0:1:20,mousedown,up:0,click:10,a:KeyA:false,a:KeyA:true,keyup:KeyA")
        );
    }

    #[test]
    fn a_captured_press_keeps_its_moves_release_and_click_on_the_capturing_element() {
        let browser = Browser::new(bundle(
            r#"<style>body { margin: 0 } p { display: block; width: 100px; height: 40px; margin: 0 }</style><p id="handle">drag</p><p id="other">other</p><p id="status"></p><script>
                const seen = [];
                const note = (entry) => { seen.push(entry); document.getElementById("status").textContent = seen.join(","); };
                const handle = document.getElementById("handle");
                document.getElementById("other").addEventListener("pointermove", () => note("other"));
                document.addEventListener("gotpointercapture", (event) => note("got:" + event.target.id + ":" + event.bubbles));
                document.addEventListener("lostpointercapture", (event) => note("lost:" + event.target.id));
                handle.addEventListener("pointerdown", (event) => {
                    let threw = "";
                    try { handle.setPointerCapture(7); } catch (error) { threw = error.name; }
                    handle.setPointerCapture(event.pointerId);
                    note("down:" + threw + ":" + handle.hasPointerCapture(1));
                    handle.onpointermove = (move) => note("move:" + move.clientY);
                    handle.onpointerup = () => note("up");
                });
                handle.addEventListener("mouseup", () => note("mouseup"));
                document.addEventListener("click", (event) => note("click:" + event.target.id));
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
        press(&mut host, (10.0, 10.0));
        drag(&mut host, (10.0, 50.0));
        drag(&mut host, (10.0, 400.0));
        release(&mut host, (10.0, 50.0));
        drag(&mut host, (12.0, 50.0));
        let document = host.browser().document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some(
                "down:NotFoundError:true,got:handle:true,move:50,move:400,up,lost:handle,mouseup,click:handle,other"
            )
        );
    }

    #[test]
    fn a_press_that_moves_four_pixels_on_a_draggable_element_starts_a_drag() {
        let browser = Browser::new(bundle(
            r#"<style>body { margin: 0 } li, p { display: block; width: 100px; height: 40px; margin: 0 }</style><ul style="margin: 0; padding: 0"><li id="card" draggable="true"><span>one</span></li></ul><p id="plain">two</p><p id="status"></p><script>
                const seen = [];
                document.addEventListener("dragstart", (event) => {
                    seen.push(event.target.id + ":" + (event instanceof DragEvent) + ":" + event.cancelable + ":" + event.clientX);
                    event.preventDefault();
                    document.getElementById("status").textContent = seen.join(",");
                });
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
        press(&mut host, (10.0, 10.0));
        drag(&mut host, (13.0, 12.0));
        drag(&mut host, (14.0, 12.0));
        drag(&mut host, (30.0, 12.0));
        release(&mut host, (30.0, 12.0));
        press(&mut host, (10.0, 50.0));
        drag(&mut host, (40.0, 50.0));
        release(&mut host, (40.0, 50.0));
        drag(&mut host, (10.0, 10.0));
        drag(&mut host, (40.0, 10.0));
        let document = host.browser().document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("card:true:true:10")
        );
    }

    #[test]
    fn devtools_input_and_screenshots_reach_the_host() {
        let browser = Browser::new(bundle(
            r#"<button id="go" style="display: block; width: 100px; height: 30px; margin: 0">go</button><input id="field" style="display: block; width: 100px; height: 30px; margin: 0; padding: 0; border: 0"><p id="status">idle</p><script>
                const status = document.getElementById("status");
                document.getElementById("go").addEventListener("click", () => { status.textContent = "clicked"; });
                document.getElementById("field").addEventListener("input", (event) => { status.textContent = event.target.value; });
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
        let mut id = 0;
        let mut call = |host: &mut EngineHost, method: &str, params: Json| {
            id += 1;
            let message = json!({ "id": id, "method": method, "params": params });
            let (answered, answer) = host
                .automation_call(&message.to_string())
                .unwrap_or_else(|| panic!("the host should answer {method}"));
            assert_eq!(answered, id);
            answer.unwrap_or_else(|error| panic!("{method} failed: {error:?}"))
        };
        let status = |host: &EngineHost| {
            let document = host.browser().document().expect("document should exist");
            let status = document
                .borrow()
                .find_element_by_id("status")
                .expect("status should exist");
            document.borrow().element_text_content(status)
        };
        let press = |kind: &str, x: f32, y: f32| json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": 1 });
        call(
            &mut host,
            "Input.dispatchMouseEvent",
            press("mousePressed", 50.0, 15.0),
        );
        call(
            &mut host,
            "Input.dispatchMouseEvent",
            press("mouseReleased", 50.0, 15.0),
        );
        assert_eq!(status(&host).as_deref(), Some("clicked"));

        call(
            &mut host,
            "Input.dispatchMouseEvent",
            press("mousePressed", 50.0, 45.0),
        );
        call(
            &mut host,
            "Input.dispatchMouseEvent",
            press("mouseReleased", 50.0, 45.0),
        );
        for key in ["d", "e"] {
            let down = json!({ "type": "keyDown", "key": key, "code": "", "text": key });
            call(&mut host, "Input.dispatchKeyEvent", down);
            call(
                &mut host,
                "Input.dispatchKeyEvent",
                json!({ "type": "keyUp", "key": key }),
            );
        }
        let backspace =
            json!({ "type": "rawKeyDown", "key": "Backspace", "code": "Backspace", "text": "" });
        call(&mut host, "Input.dispatchKeyEvent", backspace);
        call(&mut host, "Input.insertText", json!({ "text": "xy" }));
        assert_eq!(status(&host).as_deref(), Some("dxy"));

        // enter types no carriage return into a single-line field
        let enter = json!({ "type": "keyDown", "key": "Enter", "code": "Enter", "text": "\r" });
        call(&mut host, "Input.dispatchKeyEvent", enter);
        call(
            &mut host,
            "Input.dispatchKeyEvent",
            json!({ "type": "keyUp", "key": "Enter", "code": "Enter" }),
        );
        assert_eq!(status(&host).as_deref(), Some("dxy"));

        let metrics = call(&mut host, "Page.getLayoutMetrics", json!({}));
        // the status paragraph overflows, so the root's bar takes its width from the client
        let client = metrics["cssLayoutViewport"]["clientWidth"]
            .as_f64()
            .expect("a client width");
        assert!(client < 160.0, "{client}");
        assert_eq!(metrics["cssVisualViewport"]["scale"], 1);
        // the status paragraph runs past the viewport's foot
        assert!(metrics["cssContentSize"]["height"].as_f64() > Some(100.0));

        let shot = |host: &mut EngineHost,
                    call: &mut dyn FnMut(&mut EngineHost, &str, Json) -> Json,
                    params: Json| {
            let answer = call(host, "Page.captureScreenshot", params);
            let data = answer["data"].as_str().expect("a screenshot carries data");
            let (_, bytes) =
                crate::image::decode_data_url(&format!("data:image/png;base64,{data}"))
                    .expect("the data should be base64");
            let image = crate::png::decode_png(&bytes).expect("the data should be a png");
            (image.width, image.height)
        };
        assert_eq!(shot(&mut host, &mut call, json!({})), (160, 100));
        let clip = json!({ "clip": { "x": 0, "y": 0, "width": 80, "height": 40, "scale": 1 } });
        assert_eq!(shot(&mut host, &mut call, clip), (80, 40));
        let zoomed = json!({ "clip": { "x": 10, "y": 10, "width": 20, "height": 10, "scale": 2 } });
        assert_eq!(shot(&mut host, &mut call, zoomed), (40, 20));
        // a clip on the last half pixel takes the edge pixel, where it used to panic
        let edge =
            json!({ "clip": { "x": 159.5, "y": 99.5, "width": 1, "height": 1, "scale": 1 } });
        assert_eq!(shot(&mut host, &mut call, edge), (1, 1));
        // a runaway scale is refused instead of overflowing the image size
        let message = json!({ "id": 99, "method": "Page.captureScreenshot", "params": { "clip": { "x": 0, "y": 0, "width": 10, "height": 10, "scale": 1e9 } } });
        let (_, answer) = host
            .automation_call(&message.to_string())
            .expect("the host answers screenshots");
        assert!(answer.is_err());
        assert!(
            host.automation_call(r#"{"id":9,"method":"Page.navigate","params":{}}"#)
                .is_none()
        );
    }

    #[test]
    fn a_prevented_keydown_types_nothing_and_click_counts_come_from_the_protocol() {
        let browser = Browser::new(bundle(
            r#"<input id="field" style="display: block; width: 100px; height: 30px; margin: 0; padding: 0; border: 0"><p id="status"></p><script>
                const status = document.getElementById("status");
                const field = document.getElementById("field");
                field.addEventListener("keydown", (event) => { if (event.key === "x") event.preventDefault(); });
                field.addEventListener("input", () => { status.textContent = field.value; });
                field.addEventListener("dblclick", () => { status.textContent += "|dbl"; });
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
        let status = |host: &EngineHost| {
            let document = host.browser().document().expect("document should exist");
            let status = document
                .borrow()
                .find_element_by_id("status")
                .expect("status");
            document
                .borrow()
                .element_text_content(status)
                .unwrap_or_default()
        };
        let call = |host: &mut EngineHost, method: &str, params: Json| {
            let message = json!({ "id": 1, "method": method, "params": params });
            let (_, answer) = host
                .automation_call(&message.to_string())
                .expect("the host answers input");
            answer.unwrap_or_else(|error| panic!("{method} failed: {error:?}"));
        };
        let click = |count: u64| {
            [
                json!({ "type": "mousePressed", "x": 50, "y": 15, "button": "left", "clickCount": count }),
                json!({ "type": "mouseReleased", "x": 50, "y": 15, "button": "left", "clickCount": count }),
            ]
        };
        for event in click(1) {
            call(&mut host, "Input.dispatchMouseEvent", event);
        }
        for key in ["a", "x", "b"] {
            call(
                &mut host,
                "Input.dispatchKeyEvent",
                json!({ "type": "keyDown", "key": key, "text": key }),
            );
            call(
                &mut host,
                "Input.dispatchKeyEvent",
                json!({ "type": "keyUp", "key": key }),
            );
        }
        // the window path: a prevented keydown swallows the text that follows it
        host.input(InputEvent::Key {
            key: "x".to_string(),
            code: "KeyX".to_string(),
            modifiers: KeyModifiers::default(),
            repeat: false,
            pressed: true,
        })
        .expect("key");
        host.input(InputEvent::TextInput {
            text: "x".to_string(),
        })
        .expect("text");
        assert_eq!(status(&host), "ab");
        // a second click at the same spot with clickCount 1 is no double click, as in chrome
        for event in click(1).into_iter().chain(click(2)) {
            call(&mut host, "Input.dispatchMouseEvent", event);
        }
        assert_eq!(status(&host), "ab|dbl");
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
            document.borrow().element_text_content(status).as_deref(),
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
            document.borrow().element_text_content(status).as_deref(),
            Some("0|3|backward|true:true:false:true")
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
            document.borrow().element_text_content(status).as_deref(),
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

    fn status_text(host: &EngineHost) -> String {
        let document = host.browser().document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        document
            .borrow()
            .element_text_content(status)
            .unwrap_or_default()
    }

    #[test]
    fn transitions_report_run_start_end_and_cancel_in_order() {
        let browser = Browser::new(bundle(
            r#"<style>#box { width: 40px; height: 20px; background: #000000; transition: all 100ms linear; } #box.wide { width: 200px; transform: rotate(10deg); }</style><div id="box"></div><p id="status"></p><script>
                const box = document.getElementById("box");
                const status = document.getElementById("status");
                for (const kind of ["transitionrun", "transitionstart", "transitionend", "transitioncancel"]) {
                    box.addEventListener(kind, (event) => {
                        if (event.propertyName === "width") {
                            status.textContent = status.textContent + kind + ":" + event.elapsedTime + "|";
                        }
                    });
                }
            </script>"#,
        ));
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 320.0,
                height: 120.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        let set_class = |host: &mut EngineHost, class: &str| {
            let document = host.browser().document().expect("document");
            let path = document.borrow().find_element_by_id("box").expect("box");
            assert!(
                document
                    .borrow_mut()
                    .set_attribute(path, "class", class.to_string())
            );
        };
        set_class(&mut host, "wide");
        host.input(InputEvent::AdvanceTime(10)).expect("tick");
        assert_eq!(status_text(&host), "transitionrun:0|transitionstart:0|");
        // halfway the width and the rotation are both in flight
        host.input(InputEvent::AdvanceTime(50)).expect("tick");
        let layout = host.browser().layout(host.viewport()).expect("layout");
        let width = layout.body().children[0].rect.width;
        assert!(width > 60.0 && width < 180.0, "{width}");
        assert!(layout.body().children[0].transform.is_some());
        host.input(InputEvent::AdvanceTime(100)).expect("tick");
        assert_eq!(
            status_text(&host),
            "transitionrun:0|transitionstart:0|transitionend:0.1|"
        );
        // reversing mid-flight cancels the running transition
        set_class(&mut host, "");
        host.input(InputEvent::AdvanceTime(10)).expect("tick");
        set_class(&mut host, "wide");
        host.input(InputEvent::AdvanceTime(30)).expect("tick");
        assert!(
            status_text(&host).contains("transitioncancel:"),
            "{}",
            status_text(&host)
        );
    }

    #[test]
    fn stepped_animations_sleep_until_their_next_step_and_unused_keyframes_never_wake() {
        let page = |rules: &str| {
            let mut host = EngineHost::new(
                Browser::new(bundle(&format!(
                    "<style>@keyframes blink {{ from {{ background: #ff0000 }} to {{ background: #0000ff }} }} #dot {{ width: 20px; height: 20px }} {rules}</style><div id=\"dot\"></div>"
                ))),
                Size {
                    width: 40.0,
                    height: 40.0,
                },
            );
            host.navigate("https://app.ceri.invalid/")
                .expect("page should load");
            host
        };
        let color = |host: &mut EngineHost| {
            let frame = host
                .render()
                .expect("render")
                .expect("a changed page renders");
            let pixel =
                &frame.canvas.rgba_pixels()[(5 * frame.canvas.width() as usize + 5) * 4..][..3];
            [pixel[0], pixel[1], pixel[2]]
        };
        let mut blinking = page("#dot { animation: blink 1s steps(2, end) infinite }");
        assert_eq!(color(&mut blinking), [255, 0, 0]);
        assert_eq!(blinking.next_wake(), Some(500));
        blinking.input(InputEvent::AdvanceTime(499)).expect("tick");
        assert_eq!(blinking.next_wake(), Some(1));
        blinking.input(InputEvent::AdvanceTime(1)).expect("tick");
        assert_eq!(
            color(&mut blinking),
            [128, 0, 128],
            "the second of two steps is halfway"
        );
        assert_eq!(blinking.next_wake(), Some(500));
        let mut unused = page("");
        color(&mut unused);
        assert_eq!(
            unused.next_wake(),
            None,
            "keyframes no element runs never wake the page"
        );
        assert!(!unused.browser().has_active_animations());
    }

    #[test]
    fn animations_start_when_applied_and_report_iterations_and_end() {
        let browser = Browser::new(bundle(
            r#"<style>@keyframes grow { from { width: 10px } to { width: 110px } } #bar { height: 10px; width: 10px; } #bar.run { animation: grow 100ms linear 2; }</style><div id="bar"></div><p id="status"></p><script>
                const bar = document.getElementById("bar");
                const status = document.getElementById("status");
                for (const kind of ["animationstart", "animationiteration", "animationend"]) {
                    bar.addEventListener(kind, (event) => {
                        status.textContent = status.textContent + kind + ":" + event.animationName + "|";
                    });
                }
            </script>"#,
        ));
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 320.0,
                height: 120.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        host.input(InputEvent::AdvanceTime(1000)).expect("tick");
        let document = host.browser().document().expect("document");
        let path = document.borrow().find_element_by_id("bar").expect("bar");
        assert!(
            document
                .borrow_mut()
                .set_attribute(path, "class", "run".to_string())
        );
        host.input(InputEvent::AdvanceTime(1)).expect("tick");
        assert_eq!(status_text(&host), "animationstart:grow|");
        // the animation runs from when the class arrived, not from page load
        host.input(InputEvent::AdvanceTime(49)).expect("tick");
        let width = host
            .browser()
            .layout(host.viewport())
            .expect("layout")
            .body()
            .children[0]
            .rect
            .width;
        assert!((width - 60.0).abs() < 3.0, "{width}");
        host.input(InputEvent::AdvanceTime(60)).expect("tick");
        host.input(InputEvent::AdvanceTime(100)).expect("tick");
        assert_eq!(
            status_text(&host),
            "animationstart:grow|animationiteration:grow|animationend:grow|"
        );
    }

    struct SharedClipboard {
        text: std::cell::RefCell<String>,
    }

    impl crate::navigation::ClipboardProvider for SharedClipboard {
        fn read_text(&self) -> Result<String, NavigationError> {
            Ok(self.text.borrow().clone())
        }

        fn write_text(&self, text: &str) -> Result<(), NavigationError> {
            *self.text.borrow_mut() = text.to_string();
            Ok(())
        }
    }

    fn prose_host(source: &str, width: f32, height: f32) -> (EngineHost, Rc<SharedClipboard>) {
        let clipboard = Rc::new(SharedClipboard {
            text: std::cell::RefCell::new(String::new()),
        });
        let browser = Browser::new(bundle(source)).with_clipboard_provider(clipboard.clone());
        let mut host = EngineHost::new(browser, Size { width, height });
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        (host, clipboard)
    }

    // the viewport point at the start or end of the run that draws a word
    fn word_point(host: &EngineHost, word: &str, at_end: bool) -> (f32, f32) {
        let layout = host
            .browser()
            .layout(host.viewport())
            .expect("layout should resolve");
        let prose = layout
            .prose_runs()
            .into_iter()
            .find(|prose| prose.run.text.trim() == word)
            .unwrap_or_else(|| panic!("a run should draw {word}"));
        let y = prose.run.y + prose.line_height() / 2.0;
        if at_end {
            (prose.run.x + prose.run.width - 0.25, y)
        } else {
            (prose.run.x + 0.25, y)
        }
    }

    fn press(host: &mut EngineHost, (x, y): (f32, f32)) {
        host.input(InputEvent::PointerDown {
            x,
            y,
            button: PointerButton::Primary,
        })
        .expect("pointer down should be accepted");
    }

    fn drag(host: &mut EngineHost, (x, y): (f32, f32)) {
        host.input(InputEvent::PointerMove { x, y })
            .expect("pointer move should be accepted");
    }

    fn release(host: &mut EngineHost, (x, y): (f32, f32)) {
        host.input(InputEvent::PointerUp {
            x,
            y,
            button: PointerButton::Primary,
        })
        .expect("pointer up should be accepted");
        host.input(InputEvent::Click { x, y })
            .expect("click should be accepted");
    }

    fn chord(host: &mut EngineHost, key: &str, modifiers: KeyModifiers) {
        host.input(InputEvent::KeyDownWithModifiers {
            key: key.to_string(),
            modifiers,
        })
        .expect("key should be accepted");
    }

    fn copied(host: &mut EngineHost, clipboard: &SharedClipboard) -> String {
        clipboard.text.borrow_mut().clear();
        chord(
            host,
            "c",
            KeyModifiers {
                control: true,
                ..KeyModifiers::default()
            },
        );
        clipboard.text.borrow().clone()
    }

    const TWO_PARAGRAPHS: &str = "<style>p { margin: 0 0 8px; font-size: 16px; }</style><script>const hidden = 'script text';</script><p>first para</p><p>second line</p>";

    #[test]
    fn a_drag_across_paragraphs_copies_the_rendered_text_and_highlights_both() {
        let (mut host, clipboard) = prose_host(TWO_PARAGRAPHS, 320.0, 120.0);
        let start = word_point(&host, "first", false);
        let end = word_point(&host, "second", true);
        press(&mut host, start);
        drag(&mut host, end);
        release(&mut host, end);
        assert_eq!(copied(&mut host, &clipboard), "first para\nsecond");

        let layout = host
            .browser()
            .layout(host.viewport())
            .expect("layout should resolve");
        assert_eq!(layout.prose_selections.len(), 2);
        let canvas = layout.paint();
        let width = canvas.width() as usize;
        let highlighted_rows = canvas
            .rgba_pixels()
            .chunks_exact(width * 4)
            .enumerate()
            .filter(|(_, row)| {
                row.as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[..3] == [170, 205, 238])
            })
            .map(|(row, _)| row as f32)
            .collect::<Vec<_>>();
        assert!(
            highlighted_rows
                .iter()
                .any(|row| (*row - start.1).abs() < 4.0)
        );
        assert!(
            highlighted_rows
                .iter()
                .any(|row| (*row - end.1).abs() < 4.0)
        );
    }

    #[test]
    fn a_backward_drag_keeps_the_press_point_as_the_anchor() {
        let (mut host, clipboard) = prose_host(TWO_PARAGRAPHS, 320.0, 120.0);
        let anchor = word_point(&host, "second", true);
        press(&mut host, anchor);
        let para = word_point(&host, "para", false);
        drag(&mut host, para);
        assert_eq!(copied(&mut host, &clipboard), "para\nsecond");
        let line_end = word_point(&host, "line", true);
        drag(&mut host, line_end);
        release(&mut host, line_end);
        assert_eq!(copied(&mut host, &clipboard), " line");
    }

    #[test]
    fn repeated_clicks_select_a_word_then_its_block_and_report_the_count() {
        let (mut host, clipboard) = prose_host(
            "<style>p { margin: 0 0 8px; font-size: 16px; }</style><p id='target'>first para</p><p id='status'>idle</p><script>
                const target = document.getElementById('target');
                const status = document.getElementById('status');
                let clicks = '';
                target.addEventListener('click', (event) => { clicks = clicks + event.detail; });
                target.addEventListener('dblclick', (event) => { status.textContent = 'dbl' + event.detail + ':' + clicks; });
            </script>",
            320.0,
            120.0,
        );
        let point = word_point(&host, "para", false);
        let point = (point.0 + 6.0, point.1);
        press(&mut host, point);
        release(&mut host, point);
        press(&mut host, point);
        release(&mut host, point);
        assert_eq!(copied(&mut host, &clipboard), "para");
        let document = host.browser().document().expect("document should exist");
        let status = document
            .borrow()
            .find_element_by_id("status")
            .expect("status should exist");
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("dbl2:12")
        );
        press(&mut host, point);
        release(&mut host, point);
        assert_eq!(copied(&mut host, &clipboard), "first para");

        // a press after the series window starts a fresh single click
        host.input(InputEvent::AdvanceTime(600))
            .expect("clock should advance");
        press(&mut host, point);
        release(&mut host, point);
        assert_eq!(copied(&mut host, &clipboard), "");
    }

    #[test]
    fn a_drag_after_a_double_click_extends_by_whole_words() {
        let (mut host, clipboard) = prose_host(TWO_PARAGRAPHS, 320.0, 120.0);
        let point = word_point(&host, "para", false);
        let point = (point.0 + 6.0, point.1);
        press(&mut host, point);
        release(&mut host, point);
        press(&mut host, point);
        let into_line = word_point(&host, "line", false);
        drag(&mut host, (into_line.0 + 6.0, into_line.1));
        release(&mut host, (into_line.0 + 6.0, into_line.1));
        assert_eq!(copied(&mut host, &clipboard), "para\nsecond line");
    }

    #[test]
    fn shift_arrows_extend_prose_by_characters_words_and_lines() {
        let (mut host, clipboard) = prose_host(TWO_PARAGRAPHS, 320.0, 120.0);
        let point = word_point(&host, "first", false);
        let point = (point.0 + 6.0, point.1);
        press(&mut host, point);
        release(&mut host, point);
        press(&mut host, point);
        release(&mut host, point);
        assert_eq!(copied(&mut host, &clipboard), "first");

        let shift = KeyModifiers {
            shift: true,
            ..KeyModifiers::default()
        };
        let word = KeyModifiers {
            shift: true,
            control: !cfg!(target_os = "macos"),
            alt: cfg!(target_os = "macos"),
            ..KeyModifiers::default()
        };
        chord(&mut host, "ArrowRight", shift);
        assert_eq!(copied(&mut host, &clipboard), "first ");
        chord(&mut host, "ArrowRight", word);
        assert_eq!(copied(&mut host, &clipboard), "first para");
        chord(&mut host, "ArrowRight", word);
        chord(&mut host, "End", shift);
        assert_eq!(copied(&mut host, &clipboard), "first para\nsecond line");
        chord(&mut host, "Home", shift);
        assert_eq!(copied(&mut host, &clipboard), "first para");
        chord(&mut host, "ArrowLeft", shift);
        assert_eq!(copied(&mut host, &clipboard), "first par");
    }

    #[test]
    fn select_all_without_focus_takes_every_rendered_line() {
        let (mut host, clipboard) = prose_host(TWO_PARAGRAPHS, 320.0, 120.0);
        chord(
            &mut host,
            "a",
            KeyModifiers {
                control: true,
                ..KeyModifiers::default()
            },
        );
        assert_eq!(copied(&mut host, &clipboard), "first para\nsecond line");
    }

    #[test]
    fn a_press_on_the_scrollbar_thumb_does_not_select_the_text_beside_it() {
        let (mut host, _) = prose_host(
            "<style>body { margin: 0; } #pane { width: 200px; height: 60px; overflow: auto; } p { margin: 0; font-size: 16px; }</style><div id='pane'><p>one</p><p>two</p><p>three</p><p>four</p><p>five</p><p>six</p></div>",
            320.0,
            120.0,
        );
        let layout = host
            .browser()
            .layout(host.viewport())
            .expect("layout should resolve");
        let thumb = (0..200)
            .rev()
            .map(|x| (x as f32 + 0.5, 4.0))
            .find(|(x, y)| layout.thumb_drag_at(*x, *y).is_some())
            .expect("the pane should paint a thumb at its top");
        press(&mut host, thumb);
        let page_selection = host
            .browser()
            .selected_text(host.viewport())
            .expect("selection should read");
        assert_eq!(page_selection, "");
    }

    #[test]
    fn transformed_prose_with_length_changing_case_maps_selection_offsets() {
        let (mut host, clipboard) = prose_host(
            "<style>p { margin: 0; font-size: 16px; text-transform: uppercase; }</style><p>straße ǆemo</p>",
            320.0,
            80.0,
        );
        let start = word_point(&host, "STRASSE", false);
        let end = word_point(&host, "ǄEMO", true);
        press(&mut host, start);
        drag(&mut host, end);
        release(&mut host, end);
        assert_eq!(copied(&mut host, &clipboard), "STRASSE ǄEMO");
    }

    #[test]
    fn a_selection_across_inline_elements_keeps_the_spaces_between_them() {
        let (mut host, clipboard) = prose_host(
            "<style>p { margin: 0; font-size: 16px; }</style><p>plain <b>bold</b> tail</p>",
            320.0,
            80.0,
        );
        let start = word_point(&host, "plain", false);
        let end = word_point(&host, "tail", true);
        press(&mut host, start);
        drag(&mut host, end);
        release(&mut host, end);
        assert_eq!(copied(&mut host, &clipboard), "plain bold tail");
        let layout = host
            .browser()
            .layout(host.viewport())
            .expect("layout should resolve");
        assert_eq!(layout.prose_selections.len(), 3);
    }

    #[test]
    fn a_devtools_pick_takes_the_click_from_the_page() {
        let browser = Browser::new(bundle(
            r#"<button id="go" style="display: block; width: 80px; height: 24px; margin: 0">run</button><p id="status">idle</p><script>
                for (const kind of ["pointerdown", "pointerup", "click"]) {
                    document.getElementById("go").addEventListener(kind, () => {
                        document.getElementById("status").textContent += " " + kind;
                    });
                }
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
        let (outgoing, messages) = std::sync::mpsc::channel();
        host.browser.attach_inspector(1, outgoing);
        let pick = r#"{"id": 1, "method": "Overlay.setInspectMode", "params": {"mode": "searchForNode", "highlightConfig": {}}}"#;
        assert_eq!(host.browser.inspect(1, pick), None);
        let (x, y) = (20.0, 10.0);
        let button = PointerButton::Primary;
        for event in [
            InputEvent::PointerMove { x, y },
            InputEvent::PointerDown { x, y, button },
            InputEvent::PointerUp { x, y, button },
            InputEvent::Click { x, y },
        ] {
            assert!(host.input(event).expect("input should apply").handled);
        }
        fn status(host: &EngineHost) -> Option<String> {
            let document = host.browser().document().expect("document should exist");
            let status = document
                .borrow()
                .find_element_by_id("status")
                .expect("status should exist");
            document.borrow().element_text_content(status)
        }
        assert_eq!(status(&host).as_deref(), Some("idle"));
        assert!(
            messages
                .try_iter()
                .any(|message| message.contains("Overlay.inspectNodeRequested"))
        );
        host.input(InputEvent::Click { x, y })
            .expect("input should apply");
        assert_eq!(status(&host).as_deref(), Some("idle click"));
    }
}
